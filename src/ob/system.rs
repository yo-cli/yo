use crate::ob::utils;
use anyhow::{anyhow, Result};
use colored::Colorize;
use inquire::{Confirm, CustomType};
use std::fs;
use std::process::Command;

/// Check if the system is Debian
pub fn check_debian() -> Result<String> {
    let os_release =
        fs::read_to_string("/etc/os-release").map_err(|_| anyhow!("Cannot read /etc/os-release"))?;

    for line in os_release.lines() {
        if line.starts_with("ID=") {
            let id = line.trim_start_matches("ID=").trim_matches('"');
            if id != "debian" {
                return Err(anyhow!("Current system is not Debian, detected: {}", id));
            }
        }
        if line.starts_with("PRETTY_NAME=") {
            let name = line.trim_start_matches("PRETTY_NAME=").trim_matches('"');
            return Ok(name.to_string());
        }
    }

    Err(anyhow!("Cannot identify system type"))
}

/// Check if running as root
#[cfg(unix)]
pub fn check_root() -> Result<()> {
    if !nix::unistd::geteuid().is_root() {
        return Err(anyhow!("Root permission required, please run with sudo"));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn check_root() -> Result<()> {
    Err(anyhow!("This command is only supported on Linux/Unix"))
}

/// Display system check results
pub fn display_system_checks() -> Result<()> {
    let os_name = check_debian()?;
    println!("[{}] System: {}", "✓".green(), os_name);

    check_root()?;
    println!("[{}] Permission: root", "✓".green());

    println!();
    Ok(())
}

/// Check if a package is installed
pub fn is_package_installed(package: &str) -> Result<bool> {
    let output = Command::new("dpkg-query")
        .args(["-W", "-f=${Status}", package])
        .output()?;

    if output.status.success() {
        let status = String::from_utf8_lossy(&output.stdout);
        Ok(status.contains("install ok installed"))
    } else {
        Ok(false)
    }
}

/// Execute apt update
pub fn apt_update() -> Result<()> {
    println!("[{}] Updating package lists...", "⟳".cyan());

    let output = Command::new("apt").args(["update", "-y"]).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("apt update failed: {}", stderr));
    }

    println!("[{}] Package lists updated", "✓".green());
    Ok(())
}

/// Install packages via apt
pub fn apt_install(packages: &[&str]) -> Result<()> {
    for package in packages {
        print!("[{}] Installing {}... ", "⟳".cyan(), package);
        std::io::Write::flush(&mut std::io::stdout())?;

        let output = Command::new("apt")
            .args(["install", "-y", package])
            .output()?;

        if output.status.success() {
            println!("{}", "✓".green());
        } else {
            println!("{}", "✗".red());
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Failed to install {}: {}", package, stderr));
        }
    }

    Ok(())
}

/// Check and install required packages
pub fn check_and_install_packages() -> Result<()> {
    let packages = vec!["iputils-clockdiff", "rpm2cpio", "alien"];

    println!("[Package Dependencies Check]\n");

    let mut missing_packages = vec![];
    for package in &packages {
        if is_package_installed(package)? {
            println!("  {} {}", "✓".green(), package);
        } else {
            println!("  {} {} [Missing]", "✗".red(), package);
            missing_packages.push(*package);
        }
    }

    if missing_packages.is_empty() {
        println!(
            "\n{} All required packages are installed.\n",
            "✓".green()
        );
        return Ok(());
    }

    println!();

    apt_update()?;

    println!();
    apt_install(&missing_packages)?;

    println!(
        "\n{} All required packages installed successfully.\n",
        "✓".green()
    );
    Ok(())
}

/// Get current SSH port
pub fn get_ssh_port() -> Result<u16> {
    let custom_config = "/etc/ssh/sshd_config.d/ssh_custom_port.conf";

    if let Ok(content) = fs::read_to_string(custom_config) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("Port ") {
                if let Ok(port) = line.strip_prefix("Port ").unwrap_or("22").trim().parse() {
                    return Ok(port);
                }
            }
        }
    }

    if let Ok(content) = fs::read_to_string("/etc/ssh/sshd_config") {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("Port ") && !line.starts_with('#') {
                if let Ok(port) = line.strip_prefix("Port ").unwrap_or("22").trim().parse() {
                    return Ok(port);
                }
            }
        }
    }

    Ok(22)
}

/// Configure SSH port
pub fn configure_ssh_port(force: bool) -> Result<()> {
    println!("[SSH Port Configuration]\n");

    let current_port = get_ssh_port()?;

    if current_port != 22 {
        println!(
            "  {} SSH port is already configured to: {}",
            "✓".green(),
            current_port
        );
        println!();
        return Ok(());
    }

    println!("  {} Current SSH port: {}", "⚠".yellow(), current_port);
    println!(
        "  {} For security, it's recommended to change SSH port from 22",
        "ℹ".cyan()
    );
    println!();

    if force {
        println!("Force mode: skipping SSH port configuration");
        return Ok(());
    }

    let should_change = Confirm::new("Do you want to change the SSH port?")
        .with_default(true)
        .prompt()
        .unwrap_or(false);

    if !should_change {
        println!("Keeping SSH port at {}", current_port);
        println!();
        return Ok(());
    }

    let new_port: u16 = CustomType::new("Enter new SSH port (1024-65535):")
        .with_default(22888)
        .with_error_message("Please enter a valid port number")
        .with_parser(&|input| {
            input
                .parse::<u16>()
                .map_err(|_| ())
                .and_then(|p| {
                    if p < 1024 {
                        Err(())
                    } else if p == 22 {
                        Err(())
                    } else {
                        Ok(p)
                    }
                })
                .map_err(|_| ())
        })
        .prompt()
        .unwrap_or(22888);

    let config_content = format!("Port {}\n", new_port);
    fs::write("/etc/ssh/sshd_config.d/ssh_custom_port.conf", config_content)?;

    println!("\n[{}] SSH port configuration written", "✓".green());

    print!("[{}] Restarting SSH service... ", "⟳".cyan());
    std::io::Write::flush(&mut std::io::stdout())?;

    let output = Command::new("systemctl")
        .args(["restart", "ssh"])
        .output()?;

    if output.status.success() {
        println!("{}", "✓".green());
        println!("\n{} SSH port changed to {}", "✓".green(), new_port);
        println!(
            "{} Please use 'ssh -p {}' to connect from now on",
            "⚠".yellow(),
            new_port
        );
        println!();
    } else {
        println!("{}", "✗".red());
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to restart SSH service: {}", stderr));
    }

    Ok(())
}

/// Get system hostname
pub fn get_hostname() -> Result<String> {
    let output = Command::new("hostname").output()?;

    if output.status.success() {
        let hostname = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(hostname)
    } else {
        Err(anyhow!("Failed to get hostname"))
    }
}

/// Configure /etc/hosts with hostname mapping
pub fn configure_hosts_file() -> Result<()> {
    println!("[Hosts File Configuration]\n");

    let hostname = get_hostname()?;
    let expected_entry = format!("127.0.0.1 {}", hostname);
    let hosts_path = "/etc/hosts";

    let content = fs::read_to_string(hosts_path).unwrap_or_default();

    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let mut found_localhost = false;
    let mut needs_update = false;
    let mut removed_public_ip = false;
    let mut lines_to_remove = vec![];

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let ip = parts[0];
        let line_hostname = parts[1];

        if line_hostname == hostname || parts[1..].contains(&hostname.as_str()) {
            if ip == "127.0.0.1" {
                found_localhost = true;
                if trimmed != expected_entry {
                    needs_update = true;
                }
            } else if !ip.starts_with("::") && !ip.starts_with("fe") && !ip.starts_with("ff") {
                lines_to_remove.push(i);
                removed_public_ip = true;
                println!(
                    "  {} Removing public/private IP mapping: {} {}",
                    "✗".red(),
                    ip,
                    hostname
                );
            }
        }
    }

    for &index in lines_to_remove.iter().rev() {
        lines.remove(index);
    }

    if found_localhost && !needs_update && !removed_public_ip {
        println!(
            "  {} 127.0.0.1 {} [Already configured]",
            "✓".green(),
            hostname
        );
        println!();
        return Ok(());
    }

    let mut modified = removed_public_ip;

    if needs_update {
        println!(
            "  {} Updating hosts entry for hostname: {}",
            "⟳".yellow(),
            hostname
        );
        let mut localhost_index = None;
        for (i, line) in lines.iter().enumerate() {
            if line.trim().starts_with("127.0.0.1") && line.contains(&hostname) {
                localhost_index = Some(i);
                break;
            }
        }
        if let Some(index) = localhost_index {
            lines[index] = expected_entry.clone();
        }
        modified = true;
    } else if !found_localhost {
        println!(
            "  {} Adding hosts entry for hostname: {}",
            "+".green(),
            hostname
        );
        lines.push(expected_entry.clone());
        modified = true;
    }

    if !modified {
        println!("  {} No changes needed", "✓".green());
        println!();
        return Ok(());
    }

    let backup = utils::backup_file(hosts_path)?;
    if !backup.is_empty() {
        println!(
            "  [{}] {} → {}",
            "Backup".cyan(),
            hosts_path,
            backup.split('/').last().unwrap_or(&backup)
        );
    }

    let new_content = lines.join("\n") + "\n";
    fs::write(hosts_path, new_content)?;

    println!("  [{}] {}", "Modified".green(), hosts_path);
    println!(
        "\n{} Hosts file configured: {}\n",
        "✓".green(),
        expected_entry
    );

    if removed_public_ip {
        println!(
            "{} Removed public/private IP mappings for security",
            "ℹ".cyan()
        );
        println!(
            "{} Only localhost (127.0.0.1) should map to hostname\n",
            "ℹ".cyan()
        );
    }

    Ok(())
}
