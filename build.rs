//! Build script. For every system it notes when the program was built (`ONEUM_BUILT`,
//! shown under Settings → About): two builds of one version are told apart by it, on a
//! phone where nothing else says which one is installed. The rest is for Windows only:
//! the program's icon and version details (assets/windows/oneum.rc) are compiled and
//! linked into `oneum.exe`, so that File Explorer, the taskbar and the Start menu show
//! the app's icon.
//!
//! The resource compiler is the one of the toolchain that links the program: `windres`
//! of mingw-w64 for the GNU target (the cross-build on a Mac: `scripts/package-windows.sh`),
//! `rc.exe` (or `llvm-rc`) for the MSVC target. Where none is found the program is built
//! without the icon and a warning says so; `WINDRES` or `RC` names one to use.
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/windows/oneum.rc");
    println!("cargo:rerun-if-changed=assets/windows/oneum.ico");
    println!("cargo:rerun-if-env-changed=WINDRES");
    println!("cargo:rerun-if-env-changed=RC");
    // Taken again whenever the program's sources change (then it is another build).
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=docs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rustc-env=ONEUM_BUILT={}", built_at());
    println!("cargo:rustc-check-cfg=cfg(oneum_widget_bridge)");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        swift_bridge();
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("set by cargo"));
    let script = root.join("assets").join("windows").join("oneum.rc");
    let version = env::var("CARGO_PKG_VERSION").expect("set by cargo");
    let numbers = version_numbers(&version);
    let gnu = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu");
    let built = if gnu {
        with_windres(&script, &out, &version, &numbers)
    } else {
        with_rc(&script, &out, &version, &numbers)
    };
    match built {
        // Only the app itself gets it (not the library tool or the tests).
        Ok(resource) => println!("cargo:rustc-link-arg-bin=oneum={}", resource.display()),
        Err(problem) => println!(
            "cargo:warning=OnEum.exe is built without its icon: {problem} \
             (set WINDRES or RC to a resource compiler)"
        ),
    }
}

/// The time of this build as "2026-11-16 00:52 UTC" (`SOURCE_DATE_EPOCH`, where a
/// build is to be repeatable, takes the clock's place).
fn built_at() -> String {
    let seconds = env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|epoch| epoch.parse::<u64>().ok())
        .or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|since| since.as_secs())
        })
        .unwrap_or(0);
    let (year, month, day) = civil_date(seconds / 86_400);
    let (hour, minute) = ((seconds % 86_400) / 3600, (seconds % 3600) / 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

/// The date of a day counted from 1970-01-01 (the usual calendar arithmetic, with
/// years that begin in March so that the leap day comes last).
fn civil_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// "0.1.0" as the four numbers a Windows version is: "0,1,0,0".
fn version_numbers(version: &str) -> String {
    let mut numbers: Vec<String> = version
        .split(['.', '-', '+'])
        .map_while(|part| part.parse::<u16>().ok())
        .map(|number| number.to_string())
        .take(4)
        .collect();
    numbers.resize(4, "0".to_string());
    numbers.join(",")
}

/// The GNU toolchain: an object file the linker takes like any other.
fn with_windres(
    script: &Path,
    out: &Path,
    version: &str,
    numbers: &str,
) -> Result<PathBuf, String> {
    let object = out.join("oneum-resources.o");
    let named = env::var("WINDRES").ok().filter(|name| !name.is_empty());
    // Built on Windows itself the compiler is plain `windres`; cross-built it carries
    // the target in its name.
    let candidates: Vec<String> = match named {
        Some(name) => vec![name],
        None => vec![
            "x86_64-w64-mingw32-windres".to_string(),
            "windres".to_string(),
        ],
    };
    let mut last = String::from("no resource compiler was tried");
    for windres in candidates {
        let run = Command::new(&windres)
            .arg("--input")
            .arg(script)
            .arg("--output-format=coff")
            .arg("--output")
            .arg(&object)
            .arg("--include-dir")
            .arg(script.parent().unwrap_or(Path::new(".")))
            .arg(format!("-DONEUM_VERSION=\\\"{version}\\\""))
            .arg(format!("-DONEUM_VERSION_NUMBERS={numbers}"))
            .output();
        match run {
            Ok(output) if output.status.success() => return Ok(object),
            Ok(output) => {
                last = format!(
                    "{windres} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Err(error) => last = format!("{windres} cannot be run: {error}"),
        }
    }
    Err(last)
}

/// Microsoft's toolchain: a `.res` file, which its linker takes as it is.
fn with_rc(script: &Path, out: &Path, version: &str, numbers: &str) -> Result<PathBuf, String> {
    let resource = out.join("oneum-resources.res");
    let named = env::var("RC").ok().filter(|name| !name.is_empty());
    let candidates: Vec<String> = match named {
        Some(name) => vec![name],
        None => vec!["rc.exe".to_string(), "llvm-rc".to_string()],
    };
    let mut last = String::from("no resource compiler was tried");
    for rc in candidates {
        let run = Command::new(&rc)
            .arg("/nologo")
            .arg(format!("/dONEUM_VERSION=\\\"{version}\\\""))
            .arg(format!("/dONEUM_VERSION_NUMBERS={numbers}"))
            .arg("/i")
            .arg(script.parent().unwrap_or(Path::new(".")))
            .arg("/fo")
            .arg(&resource)
            .arg(script)
            .output();
        match run {
            Ok(output) if output.status.success() => return Ok(resource),
            Ok(output) => {
                last = format!(
                    "{rc} failed: {}",
                    String::from_utf8_lossy(&output.stdout).trim()
                );
            }
            Err(error) => last = format!("{rc} cannot be run: {error}"),
        }
    }
    Err(last)
}

/// iPhone: the one Swift function of the app (ios/Bridge/WidgetBridge.swift, which
/// tells the system that the widgets are out of date) is compiled with the Swift
/// compiler of Xcode and linked in. Where that cannot be done the app is built without
/// it and a warning says so: the widgets then show a song only when the system asks
/// them again by itself.
fn swift_bridge() {
    println!("cargo:rerun-if-changed=ios/Bridge/WidgetBridge.swift");
    println!("cargo:rerun-if-env-changed=IPHONEOS_DEPLOYMENT_TARGET");
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("set by cargo"));
    let simulator = env::var("TARGET").is_ok_and(|target| target.ends_with("-sim"));
    let sdk = if simulator {
        "iphonesimulator"
    } else {
        "iphoneos"
    };
    let least = env::var("IPHONEOS_DEPLOYMENT_TARGET").unwrap_or_else(|_| "16.0".into());
    let triple = format!(
        "arm64-apple-ios{least}{}",
        if simulator { "-simulator" } else { "" }
    );
    let xcrun = |arguments: &[&str]| -> Option<String> {
        let told = Command::new("xcrun").args(arguments).output().ok()?;
        told.status
            .success()
            .then(|| String::from_utf8_lossy(&told.stdout).trim().to_string())
            .filter(|path| !path.is_empty())
    };
    // The Simulator does not read an app's entitlements from its signature but from a
    // section of the program itself (Xcode puts them there for it): the script that
    // builds for the Simulator names the file (scripts/ios-sim.sh, ios/build-rust.sh).
    // Without them the app has no folder to share with its widgets there.
    println!("cargo:rerun-if-env-changed=ONEUM_IOS_SIM_ENTITLEMENTS");
    if simulator
        && let Ok(file) = env::var("ONEUM_IOS_SIM_ENTITLEMENTS")
        && Path::new(&file).is_file()
    {
        println!("cargo:rerun-if-changed={file}");
        println!("cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__entitlements,{file}");
    }
    let (Some(sdk_path), Some(swiftc)) = (
        xcrun(&["--sdk", sdk, "--show-sdk-path"]),
        xcrun(&["--sdk", sdk, "--find", "swiftc"]),
    ) else {
        println!(
            "cargo:warning=no Swift compiler found: the app is built without the widgets' bridge"
        );
        return;
    };
    let object = out.join("oneum_bridge.o");
    let library = out.join("liboneum_bridge.a");
    let compiled = Command::new(&swiftc)
        .args([
            "-parse-as-library",
            "-emit-object",
            "-O",
            "-module-name",
            "OneumBridge",
        ])
        .args(["-target", &triple, "-sdk", &sdk_path])
        .arg(root.join("ios/Bridge/WidgetBridge.swift"))
        .arg("-o")
        .arg(&object)
        .status()
        .is_ok_and(|status| status.success());
    let _ = std::fs::remove_file(&library);
    let archived = compiled
        && Command::new("xcrun")
            .args(["ar", "rcs"])
            .arg(&library)
            .arg(&object)
            .status()
            .is_ok_and(|status| status.success());
    if !archived {
        println!(
            "cargo:warning=the widgets' bridge could not be compiled: the app is built without it"
        );
        return;
    }
    println!("cargo:rustc-cfg=oneum_widget_bridge");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=oneum_bridge");
    // Swift's own libraries are part of the system; the linker finds their stubs in
    // the SDK, and what a deployment target older than the compiler needs in the
    // toolchain.
    println!("cargo:rustc-link-search=native={sdk_path}/usr/lib/swift");
    if let Some(toolchain) = Path::new(&swiftc).parent().and_then(Path::parent) {
        println!(
            "cargo:rustc-link-search=native={}",
            toolchain.join("lib/swift").join(sdk).display()
        );
    }
    println!("cargo:rustc-link-lib=framework=WidgetKit");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
