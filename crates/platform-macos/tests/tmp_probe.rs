#[cfg(target_os = "macos")]
#[test]
fn probe_real_bundles() {
    for path in [
        "/Applications/Safari.app",
        "/System/Applications/Calculator.app",
        "/System/Applications/Mail.app",
        "/System/Applications/Utilities/Terminal.app",
        "/System/Applications/TextEdit.app",
    ] {
        if !std::path::Path::new(path).exists() {
            println!("{path}: absent");
            continue;
        }
        let start = std::time::Instant::now();
        let png = platform_macos::application_icon_png(path, 64).expect("an icon");
        let elapsed = start.elapsed();
        let decoded = image::load_from_memory(&png).expect("decodable");
        println!(
            "{path}: {}x{} {} bytes in {:?}",
            decoded.width(),
            decoded.height(),
            png.len(),
            elapsed
        );
        let name = path.rsplit('/').next().unwrap().replace(".app", "");
        std::fs::write(format!("/private/tmp/claude-501/-Users-bartlomiejzimny-Projects-clipboard-history/7aeb9e18-b481-4e79-91b5-98942f887abe/scratchpad/probe-{name}.png"), &png).unwrap();
    }
}
