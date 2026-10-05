fn main() {
    println!("cargo:rerun-if-changed=assets/FakeDNS.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/FakeDNS.ico");
        res.set("ProductName", "FakeDNS");
        res.set("FileDescription", "FakeDNS - DNS privacy noise generator");
        res.set("CompanyName", "FakeDNS");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed icon: {e}");
        }
    }
}
