use super::*;
use image::Rgba;

#[test]
fn roi_extending_past_display_edge_is_rejected() {
    let roi = Roi {
        x: 0.8,
        y: 0.75,
        width: 0.5,
        height: 0.5,
    };
    assert!(matches!(
        roi.to_pixels(1000, 800),
        Err(CaptureError::InvalidRoi(_))
    ));
}

#[test]
fn invalid_roi_is_rejected() {
    let roi = Roi {
        x: -0.1,
        y: 0.0,
        width: 0.5,
        height: 0.5,
    };
    assert!(matches!(
        roi.to_pixels(100, 100),
        Err(CaptureError::InvalidRoi(_))
    ));
}

#[test]
fn roi_starting_at_display_edge_is_rejected() {
    for roi in [
        Roi {
            x: 1.0,
            y: 0.0,
            width: 0.1,
            height: 0.5,
        },
        Roi {
            x: 0.0,
            y: 1.0,
            width: 0.5,
            height: 0.1,
        },
    ] {
        assert!(matches!(
            roi.to_pixels(100, 100),
            Err(CaptureError::InvalidRoi(_))
        ));
    }
}

#[test]
fn jpeg_encoding_returns_metadata_and_hash() {
    let image = RgbaImage::from_pixel(2, 3, Rgba([255, 0, 0, 255]));
    let encoded = encode_jpeg(&image, 85).expect("encoding");
    assert!(encoded.bytes.len() > 10);
    assert_eq!(encoded.width, 2);
    assert_eq!(encoded.height, 3);
    assert_eq!(encoded.sha256.len(), 64);
    assert_eq!(&encoded.bytes[..2], &[0xff, 0xd8]);
}
