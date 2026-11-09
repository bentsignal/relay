fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/relay.ico");
        res.set("ProductName", "Relay");
        res.set("FileDescription", "Relay");
        res.set("CompanyName", "Relay contributors");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
    println!("cargo:rerun-if-changed=../../assets/relay.ico");
    println!("cargo:rerun-if-env-changed=RELAY_VERSION");
}
