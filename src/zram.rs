//! Zram management — show stats and generate NixOS configuration for tuning.

use anyhow::Result;

/// Handle the `zram` CLI subcommand.
pub(crate) fn cmd_zram(
    status: bool,
    gen_config: bool,
    memory_percent: Option<u32>,
    algorithm: Option<String>,
) -> Result<()> {
    if gen_config {
        let mem_pct = memory_percent.unwrap_or(200);
        let algo = algorithm.unwrap_or_else(|| "zstd".to_string());
        let valid_algos = ["lzo", "lzo-rle", "lz4", "lz4hc", "zstd", "deflate", "842"];
        if !valid_algos.contains(&algo.as_str()) {
            return Err(anyhow::anyhow!(
                "Invalid algorithm. Valid: {}",
                valid_algos.join(", ")
            ));
        }
        let total_ram_kb: u64 = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .map(|l| l.split_whitespace().nth(1).unwrap_or("0").to_string())
            })
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let total_ram_gb = total_ram_kb as f64 / 1024.0 / 1024.0;
        println!("# Zram configuration for NixOS");
        println!("# Add this to your ~/.dracon/nixos/configuration.nix");
        println!();
        println!("  # --- ZRAM ---");
        println!("  zramSwap = {{");
        println!("    enable = true;");
        println!("    algorithm = \"{}\";", algo);
        println!(
            "    # {}% of RAM = {}GB virtual swap (based on detected {} GB RAM)",
            mem_pct,
            (mem_pct as f64 / 100.0 * total_ram_gb),
            total_ram_gb
        );
        println!("    memoryPercent = {};", mem_pct);
        println!("  }};");
        println!();
        println!("# Then rebuild: sudo nixos-rebuild switch --flake ~/.dracon/nixos#");
        return Ok(());
    }

    if status || (!gen_config) {
        let zram_path = "/sys/block/zram0";
        let mm_stat_path = format!("{}/mm_stat", zram_path);

        println!("Zram Status");
        println!("============");

        if !std::path::Path::new(zram_path).exists() {
            println!("No zram device found.");
            return Ok(());
        }

        let disksize = std::fs::read_to_string(format!("{}/disksize", zram_path))
            .map(|s| s.trim().parse::<u64>().unwrap_or(0))
            .unwrap_or(0);
        let disksize_gb = disksize / 1024 / 1024 / 1024;

        let algo = std::fs::read_to_string(format!("{}/comp_algorithm", zram_path))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let mm_stat = std::fs::read_to_string(&mm_stat_path)
            .map(|s| {
                s.split_whitespace()
                    .filter_map(|v| v.parse::<u64>().ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let orig = *mm_stat.first().unwrap_or(&0);
        let compr = *mm_stat.get(1).unwrap_or(&0);
        let mem_used = *mm_stat.get(2).unwrap_or(&0);

        let orig_gb = orig as f64 / 1024.0 / 1024.0 / 1024.0;
        let compr_gb = compr as f64 / 1024.0 / 1024.0 / 1024.0;
        let mem_used_gb = mem_used as f64 / 1024.0 / 1024.0 / 1024.0;
        let ratio = if orig > 0 {
            compr as f64 / orig as f64
        } else {
            0.0
        };

        println!();
        println!("Device: /dev/zram0");
        println!("Disksize: {} GB", disksize_gb);
        println!("Algorithm: {}", algo);
        println!();
        println!("Memory Usage:");
        println!("  Original data: {:.1} GB", orig_gb);
        println!("  Compressed:    {:.1} GB", compr_gb);
        println!("  RAM used:      {:.1} GB", mem_used_gb);
        println!(
            "  Compression ratio: {:.1}% ({:.1}x)",
            ratio * 100.0,
            if ratio > 0.0 { 1.0 / ratio } else { 0.0 }
        );
        println!();
        println!("Configuration options:");
        println!("  --gen-config           Generate NixOS configuration snippet");
        println!("  --memory-percent <N>   Set memory percent (default: 200 for 2x RAM)");
        println!("  --algorithm <algo>     Set algorithm: lzo, lz4, lz4hc, zstd (default: zstd)");
        println!();
        println!("Example - generate config for 2x RAM with zstd:");
        println!("  dracon-system zram --gen-config --memory-percent 200 --algorithm zstd");
    }

    Ok(())
}
