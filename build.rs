fn main() {
    #[cfg(windows)]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("icon.ico");
        resource.set("ProductName", "VeeType");
        resource.set("FileDescription", "VeeType voice dictation");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=Could not embed the application icon: {error}");
        }
    }
}
