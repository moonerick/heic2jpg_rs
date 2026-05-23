use std::io;
#[cfg(windows)]
use winres::WindowsResource;

fn main() -> io::Result<()> {
    #[cfg(windows)]
    {
        let mut res = WindowsResource::new();
        res.set_icon("heic2jpg.ico");
        res.compile()?;
    }
    Ok(())
}
