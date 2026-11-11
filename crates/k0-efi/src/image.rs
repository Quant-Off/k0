//! 커널 Image 헤더를 확인하는 모듈입니다.
//!
//! # Features
//! 로더에 내장한 커널이 Linux arm64 Image 형식이고, 이 로더가 전제하는 배치
//! 성질(리틀 엔디언, 임의의 물리 주소에 적재 가능)을 헤더에 적어 두었는지
//! 확인합니다. 그래뉼은 헤더의 페이지 크기 플래그에서 얻습니다. 빌드 스크립트와
//! 실행 중인 로더가 같은 함수를 씁니다.

const HEADER: usize = 64;
const MAGIC: u32 = 0x644d_5241;

/// 내장 이미지가 메모리에서 차지할 수 있는 크기의 상한입니다.
pub const MAX_IMAGE_SIZE: u64 = 64 * 1024 * 1024;

/// 이미지 헤더가 거부된 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// 헤더보다 짧음
    Truncated,
    /// "ARM\x64" 매직이 없음
    BadMagic,
    /// 빅 엔디언 커널
    BigEndian,
    /// 2MiB 경계 기준의 고정 배치를 요구하는 커널
    FixedPlacement,
    /// 페이지 크기 플래그가 4KiB도 16KiB도 아님
    BadPageSize,
    /// image_size가 파일보다 작거나 상한을 넘음
    BadSize,
}

/// 헤더에서 읽은 이미지의 성질입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    /// 메모리에서 차지하는 크기(BSS와 부트 스택 포함)
    pub image_size: u64,
    /// 커널이 쓰는 변환 그래뉼
    pub granule: u64,
}

/// 커널 Image의 헤더를 확인하는 함수입니다.
///
/// # Arguments
/// `bytes` - Image 파일 전체
///
/// # Errors
/// 형식이나 배치 성질이 맞지 않으면 `ImageError`
pub fn check_image(bytes: &[u8]) -> Result<Image, ImageError> {
    if bytes.len() < HEADER {
        return Err(ImageError::Truncated);
    }
    let le32 = |off: usize| u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
    let le64 = |off: usize| u64::from(le32(off)) | u64::from(le32(off + 4)) << 32;
    if le32(56) != MAGIC {
        return Err(ImageError::BadMagic);
    }
    let flags = le64(24);
    if flags & 1 != 0 {
        return Err(ImageError::BigEndian);
    }
    if flags & 0b1000 == 0 {
        return Err(ImageError::FixedPlacement);
    }
    let granule = match (flags >> 1) & 0b11 {
        1 => 4096,
        2 => 16384,
        _ => return Err(ImageError::BadPageSize),
    };
    let image_size = le64(16);
    if image_size < bytes.len() as u64 || image_size > MAX_IMAGE_SIZE {
        return Err(ImageError::BadSize);
    }
    Ok(Image { image_size, granule })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(image_size: u64, flags: u64) -> Vec<u8> {
        let mut h = vec![0u8; 4096];
        h[16..24].copy_from_slice(&image_size.to_le_bytes());
        h[24..32].copy_from_slice(&flags.to_le_bytes());
        h[56..60].copy_from_slice(&MAGIC.to_le_bytes());
        h
    }

    #[test]
    fn the_header_gives_size_and_granule() {
        assert_eq!(check_image(&header(0xa6000, 0xa)), Ok(Image { image_size: 0xa6000, granule: 4096 }));
        assert_eq!(check_image(&header(0xa8000, 0xc)), Ok(Image { image_size: 0xa8000, granule: 16384 }));
    }

    #[test]
    fn images_the_loader_cannot_place_are_refused() {
        assert_eq!(check_image(&[0u8; 63]), Err(ImageError::Truncated));
        let mut bad = header(0xa6000, 0xa);
        bad[56] ^= 1;
        assert_eq!(check_image(&bad), Err(ImageError::BadMagic));
        assert_eq!(check_image(&header(0xa6000, 0xb)), Err(ImageError::BigEndian));
        // 임의 배치 비트가 없으면 2MiB 경계와 text_offset을 지켜야 하는 커널임
        assert_eq!(check_image(&header(0xa6000, 0x2)), Err(ImageError::FixedPlacement));
        assert_eq!(check_image(&header(0xa6000, 0x8)), Err(ImageError::BadPageSize));
        assert_eq!(check_image(&header(0xa6000, 0xe)), Err(ImageError::BadPageSize));
        // 파일보다 작은 image_size, 상한을 넘는 image_size
        assert_eq!(check_image(&header(4095, 0xa)), Err(ImageError::BadSize));
        assert_eq!(check_image(&header(MAX_IMAGE_SIZE + 1, 0xa)), Err(ImageError::BadSize));
    }
}
