#[cfg(target_os = "linux")]
mod backend;
#[cfg(target_os = "linux")]
mod window;

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    return window::run();
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("The GNOME file and device picker requires Linux")
}
