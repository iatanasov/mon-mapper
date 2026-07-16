use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use core::str;
use edid::Descriptor;
use serde::{Deserialize, Serialize};
use tracing::{Level, debug, error, info, warn};
use tracing_subscriber::EnvFilter;
use xrandr::{Relation, ScreenResources, XHandle};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MonitorDefinition {
    #[serde(default)]
    sn: Option<String>,
    product: Option<String>,
    #[serde(skip, default)]
    available: bool,
    order: u8,
    width_px: usize,
    height_px: usize,
    #[serde(skip, default)]
    name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredConfiguration {
    presets: Vec<DisplaysConfiguration>,
    #[serde(default)]
    log_level: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DisplaysConfiguration {
    displays: Vec<MonitorDefinition>,
    profile: String,
}
impl MonitorDefinition {
    #[cfg(test)]
    pub fn from_id(
        id: &str,
        available: bool,
        order: u8,
        width_px: usize,
        height_px: usize,
    ) -> Self {
        MonitorDefinition {
            sn: Some(String::from(id)),
            product: None,
            available,
            order,
            width_px,
            height_px,
            name: None,
        }
    }
    pub fn empty(name: &str) -> Self {
        MonitorDefinition {
            sn: None,
            product: None,
            available: false,
            order: u8::MAX,
            width_px: 0,
            height_px: 0,
            name: Some(String::from(name)),
        }
    }
    pub fn xrand(&self, enable: bool, offset_x: usize, offset_y: usize) -> String {
        let Some(name) = &self.name else {
            warn!("Name is missing for {:?} {:?}", self.sn, self);
            return String::new();
        };
        if enable {
            format!(
                "--output {name} --mode {}x{} --pos {}x{}",
                self.width_px, self.height_px, offset_x, offset_y
            )
        } else {
            format!("--output {name} --off")
        }
    }
}

impl Ord for MonitorDefinition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order.cmp(&other.order)
    }
}
impl Eq for MonitorDefinition {}

impl PartialOrd for MonitorDefinition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for MonitorDefinition {
    fn eq(&self, other: &Self) -> bool {
        self.sn == other.sn
            && self.order == other.order
            && self.width_px == other.width_px
            && self.height_px == other.height_px
            && self.name == other.name
    }
}

impl DisplaysConfiguration {
    #[cfg(test)]
    pub fn new() -> Self {
        DisplaysConfiguration {
            displays: vec![],
            profile: String::new(),
        }
    }
    pub fn add(&mut self, monitor: MonitorDefinition) {
        self.displays.push(monitor);
    }
    pub fn update_from_descriptors(&mut self, desc: &SearchDescriptors, output_name: &str) -> Result<()> {
        // Try sn match first (exact, unique)
        if let Some(ref sn) = desc.sn {
            for mon in self.displays.iter_mut() {
                if mon.sn.as_deref() == Some(sn.as_str()) {
                    debug!("Matched {output_name} by sn={sn}");
                    mon.name = Some(String::from(output_name));
                    mon.available = true;
                    return Ok(());
                }
            }
        }
        // Fall back to product match (must be unambiguous)
        if let Some(ref product) = desc.product {
            let matches: Vec<usize> = self
                .displays
                .iter()
                .enumerate()
                .filter(|(_, m)| !m.available && m.product.as_deref() == Some(product.as_str()))
                .map(|(i, _)| i)
                .collect();
            if matches.len() > 1 {
                bail!(
                    "Ambiguous product match '{}' for output {}: {} config entries match. Use serial numbers instead.",
                    product, output_name, matches.len()
                );
            }
            if let Some(&idx) = matches.first() {
                debug!("Matched {output_name} by product={product}");
                self.displays[idx].name = Some(String::from(output_name));
                self.displays[idx].available = true;
                return Ok(());
            }
        }
        let id = desc.sn.as_deref().or(desc.product.as_deref()).unwrap_or("unknown");
        error!("No config match for {output_name} (sn={:?}, product={:?})", desc.sn, desc.product);
        bail!("no config match for {id}")
    }
    pub fn sort_by_order(&mut self) {
        self.displays.sort();
    }
    pub fn xrandr(&self) -> String {
        let mut offset_x = 0;
        let offset_y = 0;
        self.displays
            .iter()
            .map(|m| {
                let enable = m.width_px > 0 && m.height_px > 0;
                debug!("xrand {:?} {}", m, enable);
                let s = &m.xrand(enable, offset_x, offset_y);
                offset_x += m.width_px;
                s.to_owned()
            })
            .collect::<Vec<String>>()
            .join(" ")
    }
}
#[derive(Parser, Debug)]
struct Cli {
    #[clap(short, long, global = true)]
    config_file: Option<PathBuf>,
    #[clap(short, long, global = true)]
    log_level: Option<tracing::Level>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Get {
        #[command(subcommand)]
        sub: GetCommand,
    },
    Set {
        #[clap(short, long)]
        profile: Option<String>,
        #[clap(long)]
        print_only: bool,
    },
}

#[derive(Subcommand, Debug)]
enum GetCommand {
    Available {
        #[clap(short, long)]
        verbose: bool,
    },
    Current,
}

fn main() -> Result<()> {
    let args = Cli::parse();

    match args.command {
        Command::Get { sub } => {
            setup(args.log_level.unwrap_or(Level::INFO))?;
            match sub {
                GetCommand::Available { verbose } => get_available(verbose),
                GetCommand::Current => get_current(),
            }
        }
        Command::Set {
            profile,
            print_only,
        } => run_set(
            args.config_file.as_deref(),
            args.log_level,
            profile,
            print_only,
        ),
    }
}

fn run_set(
    config_file: Option<&Path>,
    cli_log_level: Option<Level>,
    profile: Option<String>,
    print_only: bool,
) -> Result<()> {
    let path = resolve_config_path(config_file)?;
    let config = load_config(&path)?;
    let config_level = config
        .log_level
        .as_deref()
        .map(|s| s.parse::<Level>())
        .transpose()
        .context("Invalid log_level in config file")?;
    let effective_level = cli_log_level.or(config_level).unwrap_or(Level::INFO);
    setup(effective_level)?;
    set_config(config, profile, print_only)
}

fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let config_dir = match std::env::var("XDG_CONFIG_HOME") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let home = std::env::var("HOME").context("HOME environment variable not set")?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(config_dir.join("mon-mapper").join("config.yaml"))
}

fn load_config(path: &Path) -> Result<StoredConfiguration> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    let config: StoredConfiguration = serde_yaml::from_str(&contents)
        .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
    if config.presets.is_empty() {
        bail!("Config file has no presets defined");
    }
    Ok(config)
}

struct SearchDescriptors {
    sn: Option<String>,
    product: Option<String>,
}

fn extract_descriptors(output: &xrandr::Output) -> Option<SearchDescriptors> {
    let edid = output.edid()?;
    if let nom::IResult::Done(_, info) = edid::parse(&edid) {
        let sn = info.descriptors.iter().find_map(|d| match d {
            Descriptor::SerialNumber(sn) => Some(sn.to_owned()),
            _ => None,
        });
        let product = info.descriptors.iter().find_map(|d| match d {
            Descriptor::ProductName(name) => Some(name.to_owned()),
            _ => None,
        });
        if sn.is_some() || product.is_some() {
            Some(SearchDescriptors { sn, product })
        } else {
            None
        }
    } else {
        None
    }
}

fn get_available(verbose: bool) -> Result<()> {
    let mut xhandle = XHandle::open()?;
    let outputs = xhandle.all_outputs()?;
    let screen_resources = ScreenResources::new(&mut xhandle)?;

    for output in outputs.iter().filter(|o| o.connected) {
        let desc = extract_descriptors(output);
        let serial = desc
            .as_ref()
            .and_then(|d| d.sn.as_deref())
            .unwrap_or("UNKNOWN");
        let product = desc
            .as_ref()
            .and_then(|d| d.product.as_deref())
            .unwrap_or("UNKNOWN");

        let highest = output
            .modes
            .iter()
            .filter_map(|&xid| screen_resources.mode(xid).ok())
            .max_by_key(|m| m.width as u64 * m.height as u64);

        let (max_w, max_h) = highest.map(|m| (m.width, m.height)).unwrap_or((0, 0));

        println!(
            "{}  serial={}  product={}  max={}x{}  size={}x{}mm",
            output.name, serial, product, max_w, max_h, output.mm_width, output.mm_height
        );

        if verbose
            && let Some(edid_bytes) = output.edid()
            && let nom::IResult::Done(_, info) = edid::parse(&edid_bytes)
        {
            for desc in &info.descriptors {
                match desc {
                    Descriptor::ProductName(name) => {
                        println!("  product: {name}");
                    }
                    Descriptor::SerialNumber(sn) => {
                        println!("  serial: {sn}");
                    }
                    Descriptor::UnspecifiedText(text) => {
                        println!("  text: {text}");
                    }
                    Descriptor::DetailedTiming(dt) => {
                        println!(
                            "  timing: {}x{} @{:.0}MHz",
                            dt.horizontal_active_pixels,
                            dt.vertical_active_lines,
                            dt.pixel_clock as f64 / 100.0
                        );
                    }
                    Descriptor::RangeLimits => {
                        println!("  range-limits: yes");
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn get_current() -> Result<()> {
    let mut xhandle = XHandle::open()?;
    let monitors = xhandle.monitors()?;
    let fragments: Vec<String> = monitors
        .iter()
        .flat_map(|mon| {
            mon.outputs.iter().map(move |output| {
                format!(
                    "--output {} --mode {}x{} --pos {}x{}",
                    output.name, mon.width_px, mon.height_px, mon.x, mon.y
                )
            })
        })
        .collect();
    println!("xrandr {}", fragments.join(" "));
    Ok(())
}

fn set_config(
    config: StoredConfiguration,
    profile: Option<String>,
    print_only: bool,
) -> Result<()> {
    let mut monitor_outputs = match profile {
        Some(ref name) => config
            .presets
            .into_iter()
            .find(|p| p.profile == *name)
            .ok_or_else(|| anyhow::anyhow!("Profile '{}' not found in config", name))?,
        None => config
            .presets
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No presets in config"))?,
    };

    // Must use all_outputs() here, not monitors() — monitors() only returns active
    // outputs and misses disabled-but-connected ones that need enabling.
    let mut xhandle = XHandle::open()?;
    let outputs = xhandle.all_outputs()?;

    for output in outputs.iter().filter(|o| o.connected) {
        match extract_descriptors(output) {
            Some(desc) => {
                if monitor_outputs.update_from_descriptors(&desc, &output.name).is_err() {
                    monitor_outputs.add(MonitorDefinition::empty(&output.name));
                }
            }
            None => info!("skip  {:?}", output),
        }
    }

    monitor_outputs.sort_by_order();
    if !monitor_outputs.displays.iter().any(|m| m.available) {
        bail!("No available monitor");
    }

    if print_only {
        println!("xrandr {}", monitor_outputs.xrandr());
    } else {
        apply_configuration(&mut xhandle, &monitor_outputs)?;
    }

    Ok(())
}

fn apply_configuration(xhandle: &mut XHandle, config: &DisplaysConfiguration) -> Result<()> {
    let screen_resources = ScreenResources::new(xhandle)?;

    // no_props variant skips property loading (~270 fewer X calls, ~10s faster).
    // We don't need EDID here — serial matching already happened.
    let outputs = xhandle.all_outputs_no_props()?;
    let connected: Vec<&xrandr::Output> = outputs.iter().filter(|o| o.connected).collect();

    let active_names: Vec<&str> = config
        .displays
        .iter()
        .filter(|m| m.available)
        .filter_map(|m| m.name.as_deref())
        .collect();

    // Phase 1: disable unwanted, enable needed
    let mut state_changed = false;
    for output in &connected {
        if output.current_mode.is_some() && !active_names.contains(&output.name.as_str()) {
            info!("Disabling {}", output.name);
            xhandle.disable(output)?;
            state_changed = true;
        }
    }

    for mon in config.displays.iter().filter(|m| m.available) {
        let output_name = mon
            .name
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("No output name for monitor {:?}/{:?}", mon.sn, mon.product))?;
        if let Some(output) = connected.iter().find(|o| o.name == output_name)
            && output.current_mode.is_none()
        {
            info!("Enabling {}", output_name);
            xhandle.enable(output)?;
            state_changed = true;
        }
    }

    // Phase 2: re-fetch only if state changed (enable/disable alters CRTC assignments)
    let fresh_outputs;
    let outputs_for_phase3: Vec<&xrandr::Output> = if state_changed {
        fresh_outputs = xhandle.all_outputs_no_props()?;
        fresh_outputs.iter().filter(|o| o.connected).collect()
    } else {
        connected
    };

    // Phase 3: set modes and positions
    let mut prev_output_name: Option<String> = None;
    for mon in config.displays.iter().filter(|m| m.available) {
        let output_name = mon
            .name
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("No output name for monitor {:?}/{:?}", mon.sn, mon.product))?;
        let output = outputs_for_phase3
            .iter()
            .find(|o| o.name == output_name)
            .ok_or_else(|| anyhow::anyhow!("Output '{}' not found", output_name))?;

        if let Some(current_mode_xid) = output.current_mode {
            let current = screen_resources.mode(current_mode_xid)?;
            if current.width as usize != mon.width_px || current.height as usize != mon.height_px {
                let desired = find_mode(&screen_resources, output, mon.width_px, mon.height_px)?;
                debug!(
                    "Setting mode {}x{} on {}",
                    mon.width_px, mon.height_px, output_name
                );
                xhandle.set_mode(output, &desired)?;
            }
        }

        if let Some(ref prev_name) = prev_output_name {
            let prev = outputs_for_phase3
                .iter()
                .find(|o| o.name.as_str() == prev_name.as_str())
                .ok_or_else(|| anyhow::anyhow!("Previous output '{}' not found", prev_name))?;
            debug!("Positioning {} right of {}", output_name, prev_name);
            xhandle.set_position(output, &Relation::RightOf, prev)?;
        }

        prev_output_name = Some(output_name.to_string());
    }

    info!("Configuration applied");
    Ok(())
}

fn find_mode(
    screen_resources: &ScreenResources,
    output: &xrandr::Output,
    width: usize,
    height: usize,
) -> Result<xrandr::Mode> {
    output
        .modes
        .iter()
        .filter_map(|&xid| screen_resources.mode(xid).ok())
        .find(|m| m.width as usize == width && m.height as usize == height)
        .ok_or_else(|| {
            anyhow::anyhow!("No mode {}x{} available for {}", width, height, output.name)
        })
}

fn setup(level: Level) -> Result<()> {
    tracing_subscriber::fmt::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .with_max_level(level)
        .init();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_monitor(
        sn: &str,
        order: u8,
        w: usize,
        h: usize,
        name: Option<&str>,
    ) -> MonitorDefinition {
        let mut m = MonitorDefinition::from_id(sn, name.is_some(), order, w, h);
        m.name = name.map(String::from);
        m
    }

    fn build_config(count: usize) -> DisplaysConfiguration {
        let mut cfg = DisplaysConfiguration::new();
        for i in 0..count {
            cfg.add(make_monitor(
                &format!("SN{i}"),
                i as u8,
                1920 + (i * 640),
                1080,
                Some(&format!("DP-{i}")),
            ));
        }
        cfg
    }

    #[test]
    fn xrand_enabled_output() {
        let m = make_monitor("ABC", 0, 2560, 1440, Some("DP-1"));
        assert_eq!(
            m.xrand(true, 0, 0),
            "--output DP-1 --mode 2560x1440 --pos 0x0"
        );
    }

    #[test]
    fn xrand_enabled_with_offset() {
        let m = make_monitor("ABC", 0, 2560, 1440, Some("DP-2"));
        assert_eq!(
            m.xrand(true, 2560, 0),
            "--output DP-2 --mode 2560x1440 --pos 2560x0"
        );
    }

    #[test]
    fn xrand_disabled_output() {
        let m = make_monitor("ABC", 0, 2560, 1440, Some("HDMI-1"));
        assert_eq!(m.xrand(false, 0, 0), "--output HDMI-1 --off");
    }

    #[test]
    fn xrand_missing_name_returns_empty() {
        let m = MonitorDefinition::from_id("ABC", false, 0, 2560, 1440);
        assert_eq!(m.xrand(true, 0, 0), "");
    }

    #[test]
    fn empty_monitor_has_zero_dimensions() {
        let m = MonitorDefinition::empty("DP-3");
        assert_eq!(m.width_px, 0);
        assert_eq!(m.height_px, 0);
        assert_eq!(m.name, Some(String::from("DP-3")));
        assert!(!m.available);
    }

    #[test]
    fn xrandr_zero_monitors() {
        let cfg = build_config(0);
        assert_eq!(cfg.xrandr(), "");
    }

    #[test]
    fn xrandr_single_monitor() {
        let cfg = build_config(1);
        assert_eq!(cfg.xrandr(), "--output DP-0 --mode 1920x1080 --pos 0x0");
    }

    #[test]
    fn xrandr_two_monitors_offsets_accumulate() {
        let cfg = build_config(2);
        let cmd = cfg.xrandr();
        assert!(cmd.contains("--output DP-0 --mode 1920x1080 --pos 0x0"));
        assert!(cmd.contains("--output DP-1 --mode 2560x1080 --pos 1920x0"));
    }

    #[test]
    fn xrandr_six_monitors_all_present() {
        let cfg = build_config(6);
        let cmd = cfg.xrandr();
        for i in 0..6 {
            assert!(cmd.contains(&format!("--output DP-{i}")), "missing DP-{i}");
        }
        let fragment_count = cmd.matches("--output").count();
        assert_eq!(fragment_count, 6);
    }

    #[test]
    fn xrandr_offsets_accumulate_correctly() {
        let mut cfg = DisplaysConfiguration::new();
        let widths = [1920, 2560, 3840];
        for (i, &w) in widths.iter().enumerate() {
            cfg.add(make_monitor(
                &format!("SN{i}"),
                i as u8,
                w,
                1080,
                Some(&format!("DP-{i}")),
            ));
        }
        let cmd = cfg.xrandr();
        assert!(cmd.contains("--pos 0x0"));
        assert!(cmd.contains("--pos 1920x0"));
        assert!(cmd.contains("--pos 4480x0"));
    }

    #[test]
    fn xrandr_disabled_monitors_dont_add_offset() {
        let mut cfg = DisplaysConfiguration::new();
        cfg.add(make_monitor("A", 0, 2560, 1440, Some("DP-1")));
        cfg.add(MonitorDefinition::empty("DP-2"));
        cfg.add(make_monitor("C", 2, 1920, 1080, Some("DP-3")));
        let cmd = cfg.xrandr();
        assert!(cmd.contains("--output DP-1 --mode 2560x1440 --pos 0x0"));
        assert!(cmd.contains("--output DP-2 --off"));
        assert!(cmd.contains("--output DP-3 --mode 1920x1080 --pos 2560x0"));
    }

    #[test]
    fn match_by_sn() {
        let mut cfg = DisplaysConfiguration::new();
        cfg.add(MonitorDefinition::from_id("SN123", false, 0, 2560, 1440));
        let desc = SearchDescriptors { sn: Some("SN123".into()), product: None };
        cfg.update_from_descriptors(&desc, "DP-1").unwrap();
        assert_eq!(cfg.displays[0].name, Some(String::from("DP-1")));
        assert!(cfg.displays[0].available);
    }

    #[test]
    fn match_by_product() {
        let mut cfg = DisplaysConfiguration::new();
        let mut mon = MonitorDefinition::from_id("OTHER", false, 0, 2560, 1440);
        mon.sn = None;
        mon.product = Some("LG IPS".into());
        cfg.add(mon);
        let desc = SearchDescriptors { sn: None, product: Some("LG IPS".into()) };
        cfg.update_from_descriptors(&desc, "DP-1").unwrap();
        assert_eq!(cfg.displays[0].name, Some(String::from("DP-1")));
        assert!(cfg.displays[0].available);
    }

    #[test]
    fn match_unknown_returns_err() {
        let mut cfg = DisplaysConfiguration::new();
        cfg.add(MonitorDefinition::from_id("SN123", false, 0, 2560, 1440));
        let desc = SearchDescriptors { sn: Some("UNKNOWN".into()), product: None };
        assert!(cfg.update_from_descriptors(&desc, "DP-1").is_err());
    }

    #[test]
    fn match_sn_with_multiple_monitors() {
        let mut cfg = build_config(4);
        for m in &mut cfg.displays {
            m.name = None;
            m.available = false;
        }
        let desc = SearchDescriptors { sn: Some("SN2".into()), product: None };
        cfg.update_from_descriptors(&desc, "HDMI-1").unwrap();
        assert_eq!(cfg.displays[2].name, Some(String::from("HDMI-1")));
        assert!(cfg.displays[2].available);
        assert!(cfg.displays[0].name.is_none());
        assert!(cfg.displays[1].name.is_none());
    }

    #[test]
    fn match_ambiguous_product_errors() {
        let mut cfg = DisplaysConfiguration::new();
        let mut m1 = MonitorDefinition::from_id("X", false, 0, 2560, 1440);
        m1.sn = None;
        m1.product = Some("Same Product".into());
        let mut m2 = MonitorDefinition::from_id("Y", false, 1, 2560, 1440);
        m2.sn = None;
        m2.product = Some("Same Product".into());
        cfg.add(m1);
        cfg.add(m2);
        let desc = SearchDescriptors { sn: None, product: Some("Same Product".into()) };
        assert!(cfg.update_from_descriptors(&desc, "DP-1").is_err());
    }

    #[test]
    fn sort_by_order_with_varying_counts() {
        for count in 0..=6 {
            let mut cfg = DisplaysConfiguration::new();
            for i in (0..count).rev() {
                cfg.add(make_monitor(
                    &format!("SN{i}"),
                    i as u8,
                    1920,
                    1080,
                    Some(&format!("DP-{i}")),
                ));
            }
            cfg.sort_by_order();
            let orders: Vec<u8> = cfg.displays.iter().map(|m| m.order).collect();
            let expected: Vec<u8> = (0..count as u8).collect();
            assert_eq!(orders, expected, "failed for count={count}");
        }
    }

    #[test]
    fn monitor_ordering() {
        let a = make_monitor("A", 0, 2560, 1440, None);
        let b = make_monitor("B", 1, 2560, 1440, None);
        let c = make_monitor("C", 2, 2560, 1440, None);
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn monitor_equality_ignores_available() {
        let mut a = make_monitor("A", 0, 2560, 1440, Some("DP-1"));
        let b = make_monitor("A", 0, 2560, 1440, Some("DP-1"));
        a.available = true;
        assert_eq!(a, b);
    }

    #[test]
    fn config_yaml_parses_valid_input() {
        let yaml = r#"
presets:
  - profile: test
    displays:
    - sn: "SN001"
      order: 0
      width_px: 2560
      height_px: 1440
    - sn: "SN002"
      order: 1
      width_px: 1920
      height_px: 1080
"#;
        let config: StoredConfiguration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.presets.len(), 1);
        assert_eq!(config.presets[0].displays.len(), 2);
        let d = &config.presets[0].displays[0];
        assert_eq!(d.sn.as_deref(), Some("SN001"));
        assert_eq!(d.order, 0);
        assert_eq!(d.width_px, 2560);
        assert_eq!(d.height_px, 1440);
        assert!(!d.available);
        assert!(d.name.is_none());
    }

    #[test]
    fn config_yaml_round_trip() {
        let config = StoredConfiguration {
            presets: vec![DisplaysConfiguration {
                profile: "test".into(),
                displays: vec![MonitorDefinition {
                    sn: Some("SN001".into()),
                    product: None,
                    available: false,
                    order: 0,
                    width_px: 2560,
                    height_px: 1440,
                    name: None,
                }],
            }],
            log_level: Some("DEBUG".into()),
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: StoredConfiguration = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.presets.len(), 1);
        assert_eq!(parsed.presets[0].displays[0].sn.as_deref(), Some("SN001"));
        assert_eq!(parsed.log_level.as_deref(), Some("DEBUG"));
    }

    #[test]
    fn config_yaml_all_log_levels() {
        for level_str in ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"] {
            let yaml = format!("log_level: {level_str}\npresets: []\n");
            let config: StoredConfiguration = serde_yaml::from_str(&yaml).unwrap();
            assert!(config.log_level.is_some(), "failed for {level_str}");
        }
    }

    #[test]
    fn config_yaml_case_insensitive_log_level() {
        let yaml = "log_level: debug\npresets: []\n";
        let config: StoredConfiguration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn config_yaml_missing_log_level() {
        let yaml = "presets: []\n";
        let config: StoredConfiguration = serde_yaml::from_str(yaml).unwrap();
        assert!(config.log_level.is_none());
    }

    #[test]
    fn config_yaml_null_log_level() {
        let yaml = "log_level: null\npresets: []\n";
        let config: StoredConfiguration = serde_yaml::from_str(yaml).unwrap();
        assert!(config.log_level.is_none());
    }

    #[test]
    fn config_yaml_invalid_log_level_parses_as_string() {
        let yaml = "log_level: INVALID\npresets: []\n";
        let config: StoredConfiguration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.log_level.as_deref(), Some("INVALID"));
        assert!(config.log_level.unwrap().parse::<Level>().is_err());
    }

    #[test]
    fn config_yaml_multiple_presets() {
        let yaml = r#"
presets:
  - profile: office
    displays:
    - sn: "A"
      order: 0
      width_px: 1920
      height_px: 1080
  - profile: home
    displays:
    - sn: "B"
      order: 0
      width_px: 2560
      height_px: 1440
"#;
        let config: StoredConfiguration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.presets.len(), 2);
        assert_eq!(config.presets[0].displays[0].sn.as_deref(), Some("A"));
        assert_eq!(config.presets[1].displays[0].sn.as_deref(), Some("B"));
    }

    #[test]
    fn config_yaml_varying_display_counts() {
        for count in 0..=6 {
            let displays: Vec<String> = (0..count)
                .map(|i| {
                    format!(
                        "    - sn: \"SN{i}\"\n      order: {i}\n      width_px: 1920\n      height_px: 1080"
                    )
                })
                .collect();
            let yaml = format!(
                "presets:\n  - profile: test\n    displays:\n{}\n",
                displays.join("\n")
            );
            let config: StoredConfiguration = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(
                config.presets[0].displays.len(),
                count,
                "failed for count={count}"
            );
        }
    }

    #[test]
    fn resolve_config_path_explicit_overrides_default() {
        let explicit = PathBuf::from("/custom/path/config.yaml");
        let result = resolve_config_path(Some(explicit.as_path())).unwrap();
        assert_eq!(result, explicit);
    }

    #[test]
    fn load_config_empty_presets_errors() {
        let dir = std::env::temp_dir().join("mon-mapper-test-empty");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "presets: []\n").unwrap();
        let result = load_config(&path);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("no presets"),
            "error should mention no presets"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_config_missing_file_errors() {
        let result = load_config(Path::new("/nonexistent/config.yaml"));
        assert!(result.is_err());
    }
}
