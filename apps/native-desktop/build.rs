fn main() {
    const ICON_SIZE: u32 = 32;
    const ICON_PNG: &[u8] = include_bytes!("../desktop/src-tauri/icons/32x32.png");

    println!("cargo:rerun-if-changed=../desktop/src-tauri/icons/32x32.png");

    let decoder = png::Decoder::new(std::io::Cursor::new(ICON_PNG));
    let mut reader = decoder.read_info().expect("decode canonical app icon");
    let mut rgba = vec![
        0;
        reader
            .output_buffer_size()
            .expect("canonical app icon has a finite output size")
    ];
    let info = reader
        .next_frame(&mut rgba)
        .expect("read canonical app icon pixels");
    assert_eq!((info.width, info.height), (ICON_SIZE, ICON_SIZE));
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    rgba.truncate(info.buffer_size());

    let mut argb = Vec::with_capacity(rgba.len());
    for pixel in rgba.chunks_exact(4) {
        argb.extend_from_slice(&[pixel[3], pixel[0], pixel[1], pixel[2]]);
    }

    let out_dir = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR to the build script"),
    );
    std::fs::write(out_dir.join("app-icon.rgba"), &rgba).expect("write canonical RGBA app icon");
    std::fs::write(out_dir.join("app-icon.argb"), &argb).expect("write canonical ARGB app icon");

    slint_build::compile("ui/restored.slint").expect("compile native UI");
}
