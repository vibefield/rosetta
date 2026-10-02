//! Rebuild the browser's synthetic WebP bundles using the production encoder.
//! cargo run -p rosetta-media --example fixture_images -- clients/app/test/fixtures/prompt-images
use std::{io::Cursor, path::PathBuf};
fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("fixture directory"));
    let output = root.join("optimized");
    std::fs::create_dir_all(&output).unwrap();
    let mut manifest = serde_json::Map::new();
    for (index, name) in [
        "landscape",
        "portrait",
        "retry",
        "tool-first",
        "tool-retry",
        "gallery-slow",
        "gallery-retry",
        "gallery-decode",
    ]
    .iter()
    .enumerate()
    {
        let portrait = matches!(*name, "portrait" | "gallery-slow" | "gallery-retry");
        let source = if portrait {
            "portrait.png"
        } else {
            "landscape.png"
        };
        let mut pixels = image::open(root.join(source)).unwrap().to_rgba8();
        if index > 1 {
            for y in 0..32 {
                for x in 0..32 {
                    pixels.put_pixel(x, y, image::Rgba([(index * 31) as u8, 80, 200, 255]));
                }
            }
        }
        let mut png = Cursor::new(Vec::new());
        pixels.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let prepared = rosetta_media::prepare(png.get_ref()).unwrap();
        let asset = &prepared.asset;
        std::fs::write(output.join(asset.file_name().unwrap()), &asset.bytes).unwrap();
        for variant in &asset.image_variants {
            let digest = blake3::hash(&variant.bytes).to_hex();
            std::fs::write(output.join(format!("{digest}.webp")), &variant.bytes).unwrap();
        }
        manifest.insert(
            name.to_string(),
            serde_json::json!({"asset":asset.reference(),"dimensions":prepared.dimensions}),
        );
    }
    std::fs::write(
        output.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
}
