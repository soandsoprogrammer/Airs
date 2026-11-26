fn main() {
    println!("cargo:rerun-if-changed=webkit/host.swift");
    println!("cargo:rerun-if-changed=webkit/receipt-monitor.js");
    println!("cargo:rerun-if-changed=webkit/work-list-monitor.js");
    println!("cargo:rerun-if-changed=webkit/douyin-work-list.js");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"))
        .join("airs-webkit-host");
    let architecture = std::env::var("CARGO_CFG_TARGET_ARCH").expect("target architecture");
    let target = match architecture.as_str() {
        "aarch64" => "arm64-apple-macosx15.0",
        "x86_64" => "x86_64-apple-macosx15.0",
        _ => panic!("Unsupported macOS architecture: {architecture}"),
    };
    let monitor = std::fs::read_to_string("webkit/receipt-monitor.js").expect("receipt monitor");
    let work_list =
        std::fs::read_to_string("webkit/work-list-monitor.js").expect("work list monitor");
    let match_works =
        std::fs::read_to_string("webkit/douyin-work-list.js").expect("work list matcher");
    let script = out.with_file_name("ReceiptMonitor.swift");
    std::fs::write(
        &script,
        format!(
            r##"let receiptMonitor = #"""
{monitor}
"""#
let workListMonitor = #"""
{work_list}
"""#
let matchDouyinWorks = #"""
{match_works}
"""#
"##
        ),
    )
    .expect("write receipt monitor");
    let main = out.with_file_name("main.swift");
    std::fs::copy("webkit/host.swift", &main).expect("copy WebKit host");
    let status = std::process::Command::new("swiftc")
        .args(["-O", "-target", target])
        .arg(&script)
        .arg(&main)
        .arg("-o")
        .arg(&out)
        .status()
        .expect("Cannot launch the Apple Swift compiler");
    assert!(status.success(), "Cannot build the WKWebView host");
    println!("cargo:rustc-env=AIRS_WEBKIT_HOST_BUILD={}", out.display());
}
