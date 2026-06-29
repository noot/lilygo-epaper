use epub_reader::decode_image;

#[test]
fn decodes_png_grayscale() {
    let img = decode_image(include_bytes!("fixtures/gray.png")).unwrap();
    assert_eq!((img.width(), img.height()), (8, 4));
    // left half black, right half white
    assert!(
        img.sample(0, 0) < 32,
        "left should be dark: {}",
        img.sample(0, 0)
    );
    assert!(
        img.sample(7, 0) > 224,
        "right should be light: {}",
        img.sample(7, 0)
    );
}

#[test]
fn decodes_jpeg_grayscale() {
    let img = decode_image(include_bytes!("fixtures/gray.jpg")).unwrap();
    assert_eq!((img.width(), img.height()), (8, 4));
    assert!(
        img.sample(0, 0) < 64,
        "left should be dark: {}",
        img.sample(0, 0)
    );
    assert!(
        img.sample(7, 0) > 192,
        "right should be light: {}",
        img.sample(7, 0)
    );
}

#[test]
fn rgba_alpha_composited_over_white() {
    // left half opaque black, right half fully transparent -> should read white
    let img = decode_image(include_bytes!("fixtures/rgba.png")).unwrap();
    assert!(img.sample(0, 0) < 32, "opaque black: {}", img.sample(0, 0));
    assert!(
        img.sample(7, 0) > 224,
        "transparent -> white: {}",
        img.sample(7, 0)
    );
}

#[test]
fn rejects_unknown_format() {
    assert!(decode_image(b"not an image").is_err());
}
