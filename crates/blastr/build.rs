//! Build script: the Windows icon and file properties of both programs.
//!
//! Explorer draws the first icon resource of an executable, and GPUI's Windows
//! backend loads the window and taskbar icon with
//! `LoadImageW(module, MAKEINTRESOURCE(1), IMAGE_ICON, …)`, so the icon is
//! embedded under the numeric id `1`. The compiled resource is passed to every
//! link step of this package, so `blastr.exe` and `blastr-engine.exe` (which
//! Task Manager shows) carry the same icon and version strings.
//!
//! Windows only. Elsewhere the icon comes from the packaged desktop entry, and
//! the build script does nothing.

/// The resource id the Windows backend and Explorer read the icon from.
#[cfg(windows)]
const ICON_ID: &str = "1";

/// The brand icon, relative to the package root.
const ICON_PATH: &str = "assets/blastr.ico";

fn main() {
    println!("cargo:rerun-if-changed={ICON_PATH}");

    // `CARGO_CFG_WINDOWS` describes the target that is being built, while
    // `#[cfg(windows)]` describes the machine running the build script. A
    // resource compiler is only reachable when both are Windows, and a cross
    // build quietly skips the step instead of failing to compile.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        #[cfg(windows)]
        embed();
    }
}

/// Compile the icon and the version information into the executables.
#[cfg(windows)]
fn embed() {
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon_with_id(ICON_PATH, ICON_ID)
        .set("ProductName", "Blastr")
        .set("FileDescription", "Blastr — change impact analysis")
        .set("CompanyName", "Matas Steponavičius")
        .set("LegalCopyright", "MIT OR Apache-2.0");
    resource
        .compile()
        .expect("compiling the Windows icon and version resource");
}
