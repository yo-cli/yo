use crate::ob::config::{ipv6::Ipv6Config, oceanbase::OceanBaseConfig, sysctl::SysctlConfig};
use crate::ob::config::{ConfigFile, ConfigItem, ConfigStatus};
use crate::ob::system;
use crate::ob::utils;
use anyhow::Result;
use colored::Colorize;
use inquire::{Confirm, Select};

pub fn run(force: bool) -> Result<()> {
    // System checks
    system::display_system_checks()?;

    // Configure SSH port (first step for security)
    system::configure_ssh_port(force)?;

    // Configure hosts file with hostname mapping
    system::configure_hosts_file()?;

    // Check and install required packages
    system::check_and_install_packages()?;

    // Prepare configurations
    let mut oceanbase_config = OceanBaseConfig::new();
    let mut sysctl_config = SysctlConfig::new();
    let mut ipv6_config = Ipv6Config::new();

    // Check existing configurations
    oceanbase_config.check()?;
    sysctl_config.check()?;
    ipv6_config.check()?;

    // Collect configurations that need updates
    let oceanbase_items = oceanbase_config.expected_configs();
    let sysctl_items = sysctl_config.expected_configs();
    let ipv6_items = ipv6_config.expected_configs();

    let oceanbase_needs_update: Vec<_> = oceanbase_items
        .iter()
        .filter(|item| item.needs_update())
        .collect();
    let sysctl_needs_update: Vec<_> = sysctl_items
        .iter()
        .filter(|item| item.needs_update())
        .collect();
    let ipv6_needs_update: Vec<_> = ipv6_items
        .iter()
        .filter(|item| item.needs_update())
        .collect();

    // Check if any configuration needs update
    if oceanbase_needs_update.is_empty()
        && sysctl_needs_update.is_empty()
        && ipv6_needs_update.is_empty()
    {
        println!(
            "{} All configurations are already set correctly.",
            "✓".green()
        );
        return Ok(());
    }

    // Display configurations that need updates
    println!("Configurations to be modified:\n");

    if !oceanbase_needs_update.is_empty() {
        println!("{}:", oceanbase_config.path());
        display_config_changes(&oceanbase_needs_update);
        println!();
    }

    if !sysctl_needs_update.is_empty() {
        println!("{}:", sysctl_config.path());
        display_sysctl_changes(&sysctl_needs_update);
        println!();
    }

    if !ipv6_needs_update.is_empty() {
        println!("{}:", ipv6_config.path());
        display_sysctl_changes(&ipv6_needs_update);
        println!();
    }

    // Force mode
    if force {
        println!("Force mode: skip confirmations, overwrite all conflicts\n");
    } else {
        // Ask to continue
        let confirm = Confirm::new("Continue?")
            .with_default(false)
            .prompt()
            .unwrap_or(false);

        if !confirm {
            println!("Operation cancelled");
            return Ok(());
        }

        // Handle conflicts
        if !handle_conflicts(
            &oceanbase_needs_update,
            &sysctl_needs_update,
            &ipv6_needs_update,
        )? {
            println!("Operation cancelled");
            return Ok(());
        }
    }

    // Apply modifications
    println!();

    // Backup and modify OceanBase limits config
    if !oceanbase_needs_update.is_empty() {
        let backup = utils::backup_file(oceanbase_config.path())?;
        if !backup.is_empty() {
            println!(
                "[{}] {} → {}",
                "Backup".cyan(),
                oceanbase_config.path(),
                backup.split('/').last().unwrap_or(&backup)
            );
        }
        oceanbase_config.apply(force)?;
        println!("[{}] {}", "Modified".green(), oceanbase_config.path());
    }

    // Backup and modify sysctl.conf
    if !sysctl_needs_update.is_empty() {
        let backup = utils::backup_file(sysctl_config.path())?;
        if !backup.is_empty() {
            println!(
                "[{}] {} → {}",
                "Backup".cyan(),
                sysctl_config.path(),
                backup.split('/').last().unwrap_or(&backup)
            );
        }
        sysctl_config.apply(force)?;
        println!("[{}] {}", "Modified".green(), sysctl_config.path());
    }

    // Backup and modify IPv6 config
    if !ipv6_needs_update.is_empty() {
        let backup = utils::backup_file(ipv6_config.path())?;
        if !backup.is_empty() {
            println!(
                "[{}] {} → {}",
                "Backup".cyan(),
                ipv6_config.path(),
                backup.split('/').last().unwrap_or(&backup)
            );
        }
        ipv6_config.apply(force)?;
        println!("[{}] {}", "Modified".green(), ipv6_config.path());
    }

    // Apply sysctl
    if !sysctl_needs_update.is_empty() || !ipv6_needs_update.is_empty() {
        utils::apply_sysctl_system()?;
        println!("[{}] sysctl --system", "Applied".green());
    }

    // Verify sysctl
    println!();
    verify_sysctl_settings()?;

    println!("\n{} Done!", "✓".green());
    println!("\nTo apply the changes:");
    println!("  1. For sysctl: Already applied {}", "✓".green());
    println!("  2. For limits: Please re-login or run:");
    println!("     exec su - $USER");
    println!("\nVerify with: yo-ob check");

    Ok(())
}

fn display_config_changes(items: &[&ConfigItem]) {
    for item in items {
        match item.status {
            ConfigStatus::Missing => {
                println!("  {} {} {}", "+".green(), item.key, item.value);
            }
            ConfigStatus::Conflict => {
                println!(
                    "  {} {} {} → {}",
                    "!".yellow(),
                    item.key,
                    item.current_value.as_deref().unwrap_or(""),
                    item.value
                );
            }
            ConfigStatus::Exists => {}
        }
    }
}

fn display_sysctl_changes(items: &[&ConfigItem]) {
    for item in items {
        match item.status {
            ConfigStatus::Missing => {
                println!("  {} {} = {}", "+".green(), item.key, item.value);
            }
            ConfigStatus::Conflict => {
                println!(
                    "  {} {} = {} → {}",
                    "!".yellow(),
                    item.key,
                    item.current_value.as_deref().unwrap_or(""),
                    item.value
                );
            }
            ConfigStatus::Exists => {}
        }
    }
}

fn handle_conflicts(
    oceanbase_items: &[&ConfigItem],
    sysctl_items: &[&ConfigItem],
    ipv6_items: &[&ConfigItem],
) -> Result<bool> {
    let mut conflicts = vec![];

    for item in oceanbase_items {
        if item.status == ConfigStatus::Conflict {
            conflicts.push(*item);
        }
    }
    for item in sysctl_items {
        if item.status == ConfigStatus::Conflict {
            conflicts.push(*item);
        }
    }
    for item in ipv6_items {
        if item.status == ConfigStatus::Conflict {
            conflicts.push(*item);
        }
    }

    if conflicts.is_empty() {
        return Ok(true);
    }

    println!();

    for conflict in conflicts {
        let current = conflict.current_value.as_deref().unwrap_or("");
        println!(
            "[{}] {}: {} → {}",
            "Conflict".yellow(),
            conflict.key,
            current,
            conflict.value
        );

        let choices = vec!["Yes", "No", "All", "Skip all"];
        let selection = Select::new("Overwrite?", choices)
            .prompt()
            .unwrap_or("No");

        match selection {
            "Yes" => continue,
            "No" => return Ok(false),
            "All" => return Ok(true),
            "Skip all" => break,
            _ => return Ok(false),
        }
    }

    Ok(true)
}

fn verify_sysctl_settings() -> Result<()> {
    let settings = vec![
        ("fs.aio-max-nr", "1048576"),
        ("vm.max_map_count", "655360"),
    ];

    for (key, expected) in settings {
        if utils::verify_sysctl(key, expected)? {
            println!("[{}] {} = {}", "Verified".green(), key, expected);
        } else {
            println!("[{}] {} verification failed", "✗".red(), key);
        }
    }

    Ok(())
}
