#!/usr/bin/env python3
"""디바이스 트리 블롭을 고쳐 적대적인 부팅 입력을 만듭니다.

사용법: tools/fdt-patch.py <입력.dtb> <출력.dtb> <편집> [<편집> ...]

편집
  prop:<노드 이름>:<속성 이름>:<셀>[,<셀>...]
      그 이름의 노드에 32비트 셀들로 된 속성을 더함(셀은 16진수)
  memory:<시작>:<크기>
      루트 아래에 memory 노드를 하나 더함(주소와 크기는 셀 둘씩, 16진수)

부팅 매트릭스가 QEMU의 디바이스 트리를 떠서 한 군데만 바꾼 것으로 부팅할 때
씁니다. dtc 없이 돌도록 구조 블록을 직접 고칩니다. QEMU는 -dtb로 받은 파일의
크기를 두 배로 늘려 싣기 때문에, 커널의 DTB 크기 상한을 넘지 않게 쓰인 데까지만
남깁니다. 표준 라이브러리만 씁니다.
"""
import struct
import sys

MAGIC = 0xD00DFEED
BEGIN_NODE, END_NODE, PROP, NOP, END = 1, 2, 3, 4, 9


def pad(data):
    return data + b"\0" * (-len(data) % 4)


class Tree:
    def __init__(self, blob):
        if len(blob) < 40 or struct.unpack(">I", blob[:4])[0] != MAGIC:
            sys.exit("fdt-patch: not a device tree blob")
        head = struct.unpack(">10I", blob[:40])
        _, total, off_struct, off_strings, off_rsv, self.version, self.last, self.cpu, size_strings, size_struct = head
        ends = (off_struct + size_struct, off_strings + size_strings)
        if total > len(blob) or max(ends) > total or not 40 <= off_rsv <= off_struct:
            sys.exit("fdt-patch: the header points outside the blob")
        self.rsv = blob[off_rsv:off_struct]
        self.struct = bytearray(blob[off_struct:off_struct + size_struct])
        self.strings = bytearray(blob[off_strings:off_strings + size_strings])

    def name_offset(self, name):
        key = name.encode() + b"\0"
        at = 0
        while True:
            at = self.strings.find(key, at)
            if at < 0:
                break
            if at == 0 or self.strings[at - 1] == 0:
                return at
            at += 1
        self.strings += key
        return len(self.strings) - len(key)

    def prop(self, name, value):
        return struct.pack(">3I", PROP, len(value), self.name_offset(name)) + pad(value)

    def walk(self):
        """(토큰의 위치, 토큰, 깊이, 노드 이름, 토큰 다음 위치)를 차례로 줍니다."""
        at, depth = 0, 0
        while at < len(self.struct):
            token, = struct.unpack_from(">I", self.struct, at)
            if token == BEGIN_NODE:
                end = self.struct.index(b"\0", at + 4)
                name = self.struct[at + 4:end].decode()
                after = at + 4 + len(pad(bytes(self.struct[at + 4:end + 1])))
                yield at, token, depth, name, after
                depth += 1
            elif token == END_NODE:
                depth -= 1
                yield at, token, depth, None, at + 4
                after = at + 4
            elif token == PROP:
                size, = struct.unpack_from(">I", self.struct, at + 4)
                after = at + 12 + len(pad(b"\0" * size))
                yield at, token, depth, None, after
            elif token == NOP:
                after = at + 4
            elif token == END:
                return
            else:
                sys.exit("fdt-patch: unknown token %#x at %#x" % (token, at))
            at = after

    def add_prop(self, node, name, value):
        hits = [after for _, token, _, n, after in self.walk() if token == BEGIN_NODE and n == node]
        if len(hits) != 1:
            sys.exit("fdt-patch: expected exactly one node named %r, found %d" % (node, len(hits)))
        at = hits[0]
        # 같은 이름의 속성이 이미 있으면 둘이 되므로 거부함. 노드 자신의 속성은 자식
        # 노드보다 앞에 오므로 속성이 아닌 것이 나오면 멈춤
        key = name.encode() + b"\0"
        for pos, token, _, _, _ in self.walk():
            if pos < at:
                continue
            if token != PROP:
                break
            offset, = struct.unpack_from(">I", self.struct, pos + 8)
            if self.strings[offset:offset + len(key)] == key:
                sys.exit("fdt-patch: node %r already has a property %r" % (node, name))
        self.struct[at:at] = self.prop(name, value)

    def add_memory(self, base, size):
        # 루트의 속성 뒤, 첫 자식 노드 앞에 넣음(속성은 자식보다 앞에 와야 함)
        # (자식의 시작은 깊이 1로, 자식이 없는 루트의 끝은 깊이 0으로 나옴)
        spots = [pos for pos, token, depth, _, _ in self.walk() if (depth, token) in ((1, BEGIN_NODE), (0, END_NODE))]
        if not spots:
            sys.exit("fdt-patch: the tree has no root node")
        at = spots[0]
        name = b"memory@%x\0" % base
        node = struct.pack(">I", BEGIN_NODE) + pad(name)
        node += self.prop("device_type", b"memory\0")
        node += self.prop("reg", struct.pack(">QQ", base, size))
        node += struct.pack(">I", END_NODE)
        self.struct[at:at] = node

    def blob(self):
        off_rsv = 40
        off_struct = off_rsv + len(self.rsv)
        off_strings = off_struct + len(self.struct)
        total = (off_strings + len(self.strings) + 7) & ~7
        head = struct.pack(
            ">10I", MAGIC, total, off_struct, off_strings, off_rsv, self.version, self.last, self.cpu,
            len(self.strings), len(self.struct),
        )
        body = head + self.rsv + bytes(self.struct) + bytes(self.strings)
        return body + b"\0" * (total - len(body))


def main():
    if len(sys.argv) < 4:
        sys.exit(__doc__.strip())
    with open(sys.argv[1], "rb") as f:
        tree = Tree(f.read())
    for edit in sys.argv[3:]:
        kind, _, rest = edit.partition(":")
        if kind == "prop":
            node, name, cells = rest.split(":")
            tree.add_prop(node, name, b"".join(struct.pack(">I", int(c, 16)) for c in cells.split(",")))
        elif kind == "memory":
            base, size = (int(v, 16) for v in rest.split(":"))
            tree.add_memory(base, size)
        else:
            sys.exit("fdt-patch: unknown edit %r" % edit)
    with open(sys.argv[2], "wb") as f:
        f.write(tree.blob())


if __name__ == "__main__":
    main()
