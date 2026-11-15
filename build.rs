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
