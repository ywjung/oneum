//! What the running app holds of memory, for finding out why a phone runs out of it.
//!
//! A phone ended the app again and again for lack of memory (2026-11-15: the graphics
//! driver could not map memory any more, about twenty seconds after every start), and
//! nothing on the phone said what had grown. So a thread looks every few seconds at
//! the process as the system counts it (Android and Linux: `/proc/self`), at the
//! graphics memory as the graphics library's allocator counts it, and at how large the
//! picture of the glyphs has become. The latest reading is shown under Settings →
//! About, and a line goes in the log whenever something has grown a good deal
//! ("memory: …"): the log of a run that ended for lack of memory then says what grew.
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// How often the thread looks.
const EVERY: Duration = Duration::from_secs(2);
/// The system's limit of mappings for one process (`vm.max_map_count`).
pub const MAPPINGS_ALLOWED: usize = 65_530;

/// Graphics memory as the allocator of the graphics library counts it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gpu {
    /// Blocks taken from the driver, and their size in all.
    pub blocks: usize,
    pub reserved_mb: u64,
    /// Pieces of those blocks in use (buffers, textures), and their size in all.
    pub pieces: usize,
    pub used_mb: u64,
}

/// How many objects of each kind the graphics device holds, as the graphics library
/// counts them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Objects {
    pub buffers: usize,
    pub textures: usize,
    pub views: usize,
    pub bind_groups: usize,
    pub encoders: usize,
    /// Stretches of memory taken from the driver.
    pub allocations: usize,
}

impl Objects {
    fn most(&self) -> u64 {
        [
            self.buffers,
            self.textures,
            self.views,
            self.bind_groups,
            self.encoders,
            self.allocations,
        ]
        .into_iter()
        .max()
        .unwrap_or(0) as u64
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    /// Stretches of memory the process has mapped; zero where the system does not say.
    pub mappings: usize,
    /// The names most mappings carry, with how many carry them.
    pub most: Vec<(String, usize)>,
    pub virtual_mb: u64,
    pub resident_mb: u64,
    pub gpu: Option<Gpu>,
    pub objects: Option<Objects>,
    /// The picture all glyphs are kept in, in pixels, and how many of its rows of
    /// pixels are taken.
    pub atlas: [usize; 2],
    pub atlas_rows_used: usize,
    /// Frames drawn in this run.
    pub frames: u64,
}

impl fmt::Display for Reading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mappings={} virtual={}MB resident={}MB",
            self.mappings, self.virtual_mb, self.resident_mb
        )?;
        match self.gpu {
            Some(gpu) => write!(
                f,
                " gpu={} blocks {}MB, {} pieces {}MB",
                gpu.blocks, gpu.reserved_mb, gpu.pieces, gpu.used_mb
            )?,
            None => write!(f, " gpu=?")?,
        }
        if let Some(o) = self.objects {
            write!(
                f,
                " objects: buffers={} textures={} views={} groups={} encoders={} allocations={}",
                o.buffers, o.textures, o.views, o.bind_groups, o.encoders, o.allocations
            )?;
        }
        write!(
            f,
            " atlas={}x{} (rows used {}) frames={}",
            self.atlas[0], self.atlas[1], self.atlas_rows_used, self.frames
        )?;
        if !self.most.is_empty() {
            write!(f, " most:")?;
            for (name, count) in &self.most {
                write!(f, " {name}x{count}")?;
            }
        }
        Ok(())
    }
}

impl Reading {
    /// Whether this is so much more than `before` that it is worth a line in the log.
    fn grown_since(&self, before: &Reading) -> bool {
        let more = |now: u64, then: u64, step: u64| now >= then + step.max(then / 4);
        more(self.mappings as u64, before.mappings as u64, 1000)
            || more(self.resident_mb, before.resident_mb, 64)
            || more(
                self.gpu.map_or(0, |gpu| gpu.reserved_mb),
                before.gpu.map_or(0, |gpu| gpu.reserved_mb),
                32,
            )
            || more(
                self.gpu.map_or(0, |gpu| gpu.pieces as u64),
                before.gpu.map_or(0, |gpu| gpu.pieces as u64),
                200,
            )
            || more(
                self.objects.map_or(0, |objects| objects.most()),
                before.objects.map_or(0, |objects| objects.most()),
                100,
            )
            || self.atlas[1] > before.atlas[1]
    }
}

static FRAMES: AtomicU64 = AtomicU64::new(0);
static ATLAS: [AtomicUsize; 3] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];
static LATEST: Mutex<Option<Reading>> = Mutex::new(None);
static WATCHING: AtomicBool = AtomicBool::new(false);

/// Told by the UI for every frame it draws, with the size of the glyphs' picture and
/// how many of its rows of pixels are taken.
pub fn note_frame(atlas: [usize; 2], rows_used: usize) {
    FRAMES.fetch_add(1, Ordering::Relaxed);
    ATLAS[0].store(atlas[0], Ordering::Relaxed);
    ATLAS[1].store(atlas[1], Ordering::Relaxed);
    ATLAS[2].store(rows_used, Ordering::Relaxed);
}

/// Frames drawn in this run.
pub fn frames() -> u64 {
    FRAMES.load(Ordering::Relaxed)
}

/// The reading the watching thread took last (`None` before the first, or where
/// nothing watches).
pub fn latest() -> Option<Reading> {
    LATEST.lock().ok().and_then(|latest| latest.clone())
}

/// Looks now. Not for the UI thread on a phone: the list of mappings can be long.
pub fn read() -> Reading {
    let (mappings, most) = process_maps().unwrap_or_default();
    let (virtual_mb, resident_mb) = process_sizes().unwrap_or_default();
    Reading {
        mappings,
        most,
        virtual_mb,
        resident_mb,
        gpu: super::renderer::gpu_memory(),
        objects: super::renderer::gpu_objects(),
        atlas: [
            ATLAS[0].load(Ordering::Relaxed),
            ATLAS[1].load(Ordering::Relaxed),
        ],
        atlas_rows_used: ATLAS[2].load(Ordering::Relaxed),
        frames: FRAMES.load(Ordering::Relaxed),
    }
}

/// Starts the thread that looks every few seconds while the app is on screen (once;
/// later calls do nothing).
pub fn watch() {
    if WATCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new().name("memory".into()).spawn(|| {
        let mut logged: Option<Reading> = None;
        loop {
            if crate::platform::lifecycle::in_foreground() {
                let reading = read();
                if logged
                    .as_ref()
                    .is_none_or(|before| reading.grown_since(before))
                {
                    tracing::info!("memory: {reading}");
                    logged = Some(reading.clone());
                }
                if let Ok(mut latest) = LATEST.lock() {
                    *latest = Some(reading);
                }
            }
            std::thread::sleep(EVERY);
        }
    });
    if let Err(error) = spawned {
        tracing::warn!("memory is not watched: {error}");
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn process_maps() -> Option<(usize, Vec<(String, usize)>)> {
    Some(count_mappings(
        &std::fs::read_to_string("/proc/self/maps").ok()?,
    ))
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn process_maps() -> Option<(usize, Vec<(String, usize)>)> {
    None
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn process_sizes() -> Option<(u64, u64)> {
    Some(sizes(&std::fs::read_to_string("/proc/self/status").ok()?))
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn process_sizes() -> Option<(u64, u64)> {
    None
}

/// Of the system's list of a process's mappings (`/proc/<pid>/maps`): how many there
/// are, and the four names most of them carry. Numbers in a name are left out, so that
/// "thread 1" and "thread 2" count as one name, and of a file only its name is kept.
#[cfg_attr(
    not(any(target_os = "android", target_os = "linux", test)),
    allow(dead_code)
)]
fn count_mappings(maps: &str) -> (usize, Vec<(String, usize)>) {
    let mut total = 0;
    let mut names: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for line in maps.lines() {
        total += 1;
        // address perms offset dev inode [name, which may have spaces]
        let name = line.splitn(6, ' ').nth(5).map(str::trim).unwrap_or("");
        let name = name.rsplit('/').next().unwrap_or(name);
        let name: String = if name.is_empty() {
            "(unnamed)".into()
        } else {
            name.chars()
                .filter(|c| !c.is_ascii_digit() && !c.is_whitespace())
                .take(40)
                .collect()
        };
        *names.entry(name).or_default() += 1;
    }
    let mut most: Vec<(String, usize)> = names.into_iter().collect();
    most.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    most.truncate(4);
    (total, most)
}

/// The virtual and the resident size in megabytes, from `/proc/<pid>/status`.
#[cfg_attr(
    not(any(target_os = "android", target_os = "linux", test)),
    allow(dead_code)
)]
fn sizes(status: &str) -> (u64, u64) {
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
            .map_or(0, |kb| kb / 1024)
    };
    (field("VmSize:"), field("VmRSS:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_are_counted_by_what_they_are_called() {
        let maps = "\
7000000000-7000001000 rw-p 00000000 00:00 0                              [anon:scudo:secondary]
7000002000-7000003000 rw-p 00000000 00:00 0                              [anon:scudo:secondary]
7000004000-7000005000 r--s 00000000 00:10 1234                           /dmabuf:buffer 12
7000006000-7000007000 r--s 00000000 00:10 1235                           /dmabuf:buffer 13
7000008000-7000009000 r--s 00000000 00:10 1236                           /dmabuf:buffer 14
7000010000-7000011000 r-xp 00001000 fe:00 77                             /vendor/lib64/hw/vulkan.samsung.so
7000012000-7000013000 rw-p 00000000 00:00 0
7000014000-7000015000 rw-p 00000000 00:00 0                              [anon:stack_and_tls:4711]
";
        let (total, most) = count_mappings(maps);
        assert_eq!(total, 8);
        assert_eq!(
            most,
            [
                ("dmabuf:buffer".to_string(), 3),
                ("[anon:scudo:secondary]".to_string(), 2),
                ("(unnamed)".to_string(), 1),
                ("[anon:stack_and_tls:]".to_string(), 1),
            ]
        );
        assert_eq!(count_mappings(""), (0, Vec::new()));
    }

    #[test]
    fn the_sizes_of_the_process_are_read_in_megabytes() {
        let status =
            "Name:\toneum\nVmPeak:\t 9000000 kB\nVmSize:\t17275904 kB\nVmRSS:\t  242688 kB\n";
        assert_eq!(sizes(status), (16871, 237));
        assert_eq!(sizes("nothing of the kind"), (0, 0));
    }

    #[test]
    fn a_reading_is_one_line_and_growth_is_noticed() {
        let reading = Reading {
            mappings: 3278,
            most: vec![("(unnamed)".into(), 554), ("[page".into(), 520)],
            virtual_mb: 16871,
            resident_mb: 237,
            gpu: Some(Gpu {
                blocks: 5,
                reserved_mb: 320,
                pieces: 212,
                used_mb: 41,
            }),
            objects: Some(Objects {
                buffers: 9,
                textures: 12,
                views: 13,
                bind_groups: 14,
                encoders: 3,
                allocations: 5,
            }),
            atlas: [4096, 1024],
            atlas_rows_used: 812,
            frames: 2066,
        };
        assert_eq!(
            reading.to_string(),
            "mappings=3278 virtual=16871MB resident=237MB gpu=5 blocks 320MB, 212 pieces 41MB \
             objects: buffers=9 textures=12 views=13 groups=14 encoders=3 allocations=5 \
             atlas=4096x1024 (rows used 812) frames=2066 most: (unnamed)x554 [pagex520"
        );
        assert!(!reading.grown_since(&reading));
        let mut more = reading.clone();
        more.mappings = 4300;
        assert!(more.grown_since(&reading), "a thousand more mappings");
        more.mappings = 3900;
        assert!(!more.grown_since(&reading), "less than that");
        more.atlas = [4096, 2048];
        assert!(more.grown_since(&reading), "a taller picture of the glyphs");
        let mut more = reading.clone();
        more.gpu = Some(Gpu {
            reserved_mb: 400,
            ..reading.gpu.unwrap()
        });
        assert!(more.grown_since(&reading), "a quarter more graphics memory");
        let mut more = reading.clone();
        more.objects = Some(Objects {
            views: 120,
            ..reading.objects.unwrap()
        });
        assert!(more.grown_since(&reading), "objects that pile up");
    }
}
