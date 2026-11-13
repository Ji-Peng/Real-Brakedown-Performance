// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

//! Runtime CPU cache hierarchy detection and adaptive LLC slicing model.
//!
//! Detects L1d, L2, and L3 (LLC) cache capacities without architecture-specific `cpuid`
//! instructions, querying Linux sysfs first, falling back to POSIX `libc::sysconf`, and
//! finally defaulting to conservative parameters (32 KiB L1d, 512 KiB L2, 16 MiB LLC).
//!
//! Provides the closed-form analytical model to select the recommended
//! slice count $s^*$ such that the active left-node working set fits comfortably in the
//! target CPU's effective LLC budget while maintaining unit-stride streaming locality.

use std::sync::OnceLock;

/// Default fraction of detected LLC capacity budgeted for left-node submatrix residency.
///
/// Headroom factor $\gamma = 0.50$ accounts for expander graph metadata ($\approx 200\text{ KiB}$),
/// per-thread scratch accumulators, recent destination columns, and pseudo-random associativity conflicts.
pub const DEFAULT_LLC_HEADROOM_GAMMA: f64 = 0.50;

/// Minimum row count per slice ($R_{\min} = 16$ rows = 256 bytes = 4 cache lines).
///
/// Ensures unit-stride inner streaming loops maintain hardware stream prefetcher ramp-up
/// and amortizes the graph metadata streaming overhead across sufficient rows.
pub const MIN_ROWS_PER_SLICE: usize = 16;

/// Conservative default L1 data cache capacity in bytes (32 KiB).
pub const DEFAULT_L1D_BYTES: usize = 32 * 1024;

/// Conservative default L2 cache capacity in bytes (512 KiB).
pub const DEFAULT_L2_BYTES: usize = 512 * 1024;

/// Conservative default LLC / L3 cache capacity in bytes (16 MiB).
pub const DEFAULT_LLC_BYTES: usize = 16 * 1024 * 1024;

/// Source used to detect host CPU cache topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheSource {
    /// Detected via Linux sysfs `/sys/devices/system/cpu/cpu*/cache/`.
    Sysfs,
    /// Detected via POSIX `libc::sysconf`.
    Sysconf,
    /// Fallback to conservative defaults (32 KiB L1d, 512 KiB L2, 16 MiB LLC).
    Default,
}

/// Host CPU cache hierarchy capacities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheInfo {
    /// Level-1 data cache size in bytes (per core).
    pub l1d_bytes: usize,
    /// Level-2 cache size in bytes (per core).
    pub l2_bytes: usize,
    /// Last-level cache (L3 / shared domain) size in bytes.
    pub l3_bytes: usize,
    /// How the cache capacities were determined.
    pub source: CacheSource,
}

impl CacheInfo {
    /// Returns the conservative default cache hierarchy configuration.
    pub const fn conservative_default() -> Self {
        Self {
            l1d_bytes: DEFAULT_L1D_BYTES,
            l2_bytes: DEFAULT_L2_BYTES,
            l3_bytes: DEFAULT_LLC_BYTES,
            source: CacheSource::Default,
        }
    }
}

/// Parse a size string from sysfs (e.g. "48K", "512K", "16384K", "16M", "32MiB", "1048576").
///
/// Supports optional units (case-insensitive):
/// - `B`, `bytes` (scale 1)
/// - `K`, `k`, `KB`, `KiB` (scale 1024)
/// - `M`, `m`, `MB`, `MiB` (scale 1024 * 1024)
/// - `G`, `g`, `GB`, `GiB` (scale 1024 * 1024 * 1024)
pub fn parse_size(raw: &str) -> Option<usize> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    let digit_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num_part, unit_part) = s.split_at(digit_end);
    let value = num_part.parse::<usize>().ok()?;

    let unit = unit_part.trim().to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "" | "b" | "bytes" => 1usize,
        "k" | "kb" | "kib" => 1024usize,
        "m" | "mb" | "mib" => 1024usize * 1024,
        "g" | "gb" | "gib" => 1024usize * 1024 * 1024,
        _ => return None,
    };

    value.checked_mul(multiplier)
}

/// Probe Linux sysfs `/sys/devices/system/cpu/cpu*/cache/` for cache topology.
///
/// Identifies L1d, L2, and L3 cache sizes for the assigned worker cores.
pub fn detect_cache_sysfs() -> Option<CacheInfo> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        use std::path::PathBuf;

        // Try to identify the first core available to this process
        let mut target_cpu = 0usize;
        unsafe {
            let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut cpuset) == 0 {
                for i in 0..libc::CPU_SETSIZE as usize {
                    if libc::CPU_ISSET(i, &cpuset) {
                        target_cpu = i;
                        break;
                    }
                }
            }
        }

        let mut base_path = PathBuf::from(format!("/sys/devices/system/cpu/cpu{target_cpu}/cache"));
        if !base_path.is_dir() {
            // Fallback: try cpu0 up to cpu15
            let mut found = false;
            for cpu in 0..16 {
                let candidate = PathBuf::from(format!("/sys/devices/system/cpu/cpu{cpu}/cache"));
                if candidate.is_dir() {
                    base_path = candidate;
                    found = true;
                    break;
                }
            }
            if !found {
                return None;
            }
        }

        let mut l1d = None;
        let mut l2 = None;
        let mut l3 = None;

        // Inspect index0, index1, index2, index3, ...
        for index in 0..8 {
            let index_dir = base_path.join(format!("index{index}"));
            if !index_dir.is_dir() {
                continue;
            }

            let level_str = fs::read_to_string(index_dir.join("level")).ok()?;
            let level = level_str.trim().parse::<usize>().ok()?;

            let type_str = fs::read_to_string(index_dir.join("type")).ok()?;
            let cache_type = type_str.trim();

            let size_str = fs::read_to_string(index_dir.join("size")).ok()?;
            let size = parse_size(&size_str)?;

            match (level, cache_type) {
                (1, "Data" | "Unified") => l1d = Some(size),
                (2, "Data" | "Unified") => l2 = Some(size),
                (3, "Data" | "Unified") => l3 = Some(size),
                _ => {}
            }
        }

        if l1d.is_some() || l2.is_some() || l3.is_some() {
            return Some(CacheInfo {
                l1d_bytes: l1d.unwrap_or(DEFAULT_L1D_BYTES),
                l2_bytes: l2.unwrap_or(DEFAULT_L2_BYTES),
                l3_bytes: l3.unwrap_or(DEFAULT_LLC_BYTES),
                source: CacheSource::Sysfs,
            });
        }
    }

    None
}

/// Probe POSIX `libc::sysconf` for cache capacities.
pub fn detect_cache_sysconf() -> Option<CacheInfo> {
    #[cfg(unix)]
    unsafe {
        let mut l1d = None;
        let mut l2 = None;
        let mut l3 = None;

        #[cfg(target_os = "linux")]
        {
            let val = libc::sysconf(libc::_SC_LEVEL1_DCACHE_SIZE);
            if val > 0 {
                l1d = Some(val as usize);
            }
            let val = libc::sysconf(libc::_SC_LEVEL2_CACHE_SIZE);
            if val > 0 {
                l2 = Some(val as usize);
            }
            let val = libc::sysconf(libc::_SC_LEVEL3_CACHE_SIZE);
            if val > 0 {
                l3 = Some(val as usize);
            }
        }

        if l1d.is_some() || l2.is_some() || l3.is_some() {
            return Some(CacheInfo {
                l1d_bytes: l1d.unwrap_or(DEFAULT_L1D_BYTES),
                l2_bytes: l2.unwrap_or(DEFAULT_L2_BYTES),
                l3_bytes: l3.unwrap_or(DEFAULT_LLC_BYTES),
                source: CacheSource::Sysconf,
            });
        }
    }

    None
}

/// Detect the host CPU cache hierarchy in strict priority order:
/// 1. Linux sysfs `/sys/devices/system/cpu/cpu*/cache`
/// 2. POSIX `libc::sysconf` fallback
/// 3. Conservative defaults (32 KiB L1d, 512 KiB L2, 16 MiB LLC)
///
/// Results are memoized in process memory.
pub fn detect_cache() -> CacheInfo {
    static CACHE: OnceLock<CacheInfo> = OnceLock::new();
    *CACHE.get_or_init(|| {
        detect_cache_sysfs()
            .or_else(detect_cache_sysconf)
            .unwrap_or_else(CacheInfo::conservative_default)
    })
}

/// Calculate the recommended adaptive slice count $s^*$ using explicit parameters.
///
/// - `n_per_row`: Message length $k$ (number of left nodes in the first expander graph $G_1$).
/// - `n_rows`: Total number of rows in the coefficient matrix batch.
/// - `elem_bytes`: Element footprint in bytes (16 bytes for native 64-bit limbs).
/// - `llc_bytes`: Detected LLC capacity in bytes.
/// - `gamma`: Headroom scaling factor (default $\gamma = 0.50$).
///
/// Implements the closed-form selector:
/// $$\text{LLC}_{\text{budget}} = \gamma \cdot \text{LLC}_{\text{shared}}$$
/// $$s_{\text{llc}} = \text{next\_power\_of\_two}\left(\left\lceil \frac{16 N}{\text{LLC}_{\text{budget}}} \right\rceil\right)$$
/// $$s^* = \text{clamp}\left(s_{\text{llc}}, \; 1, \; \frac{k}{R_{\min}}\right)$$
pub fn adaptive_slice_count_with_params(
    n_per_row: usize,
    n_rows: usize,
    elem_bytes: usize,
    llc_bytes: usize,
    gamma: f64,
) -> usize {
    if n_rows == 0 || n_per_row == 0 || elem_bytes == 0 || llc_bytes == 0 {
        return 1;
    }

    let llc_budget = ((gamma * llc_bytes as f64).round() as usize).max(1);
    let w_left_1 = n_per_row
        .saturating_mul(n_rows)
        .saturating_mul(elem_bytes);

    let s_llc = ((w_left_1 + llc_budget - 1) / llc_budget)
        .max(1)
        .next_power_of_two();

    let s_max = (n_rows / MIN_ROWS_PER_SLICE).max(1);
    let s_max_p2 = 1usize << (usize::BITS - 1 - s_max.leading_zeros());

    s_llc.clamp(1, s_max_p2)
}

/// Calculate the recommended adaptive slice count $s^*$ using detected host cache parameters.
///
/// Evaluates the analytical model using [`detect_cache()`] and [`DEFAULT_LLC_HEADROOM_GAMMA`].
pub fn adaptive_slice_count(n_per_row: usize, n_rows: usize, elem_bytes: usize) -> usize {
    let cache = detect_cache();
    adaptive_slice_count_with_params(
        n_per_row,
        n_rows,
        elem_bytes,
        cache.l3_bytes,
        DEFAULT_LLC_HEADROOM_GAMMA,
    )
}
