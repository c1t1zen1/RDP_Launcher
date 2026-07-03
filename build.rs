fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "RDP Launcher");
        res.set("FileDescription", "RDP Launcher by c1t1zen");
        res.set("CompanyName", "c1t1zen");
        res.set("LegalCopyright", "Copyright (c) 2026 c1t1zen");
        res.set("OriginalFilename", "rdp-launcher.exe");
        res.compile().expect("failed to compile Windows resources");
        println!("cargo:rerun-if-changed=assets/app.ico");
    }
}
