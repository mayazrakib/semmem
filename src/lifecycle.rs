use std::process::Output;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::process::Command;

const SERVICE_UNIT: &str = "semmem.service";

pub async fn execute(action: &str) -> Result<Value> {
    ensure!(
        cfg!(target_os = "linux"),
        "Service lifecycle commands require Linux with a systemd user manager."
    );
    ensure!(
        matches!(action, "start" | "stop" | "status"),
        "Unknown service lifecycle action: {action}."
    );

    let initial = read_status().await?;

    if action == "status" {
        return Ok(initial);
    }

    let output = systemctl(&[action, SERVICE_UNIT]).await?;
    ensure_success(&output, action)?;

    read_status().await
}

async fn systemctl(arguments: &[&str]) -> Result<Output> {
    Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .output()
        .await
        .context("Could not execute systemctl --user. Use a Linux installation with systemd and a running user manager.")
}

fn ensure_success(output: &Output, action: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }

    let detail = String::from_utf8_lossy(&output.stderr);

    bail!(
        "systemctl --user {action} failed ({}): {}. Check that the systemd user manager is running.",
        output.status,
        detail.trim().trim_end_matches('.'),
    )
}

async fn read_status() -> Result<Value> {
    let output = systemctl(&[
        "show",
        SERVICE_UNIT,
        "--property=LoadState",
        "--property=ActiveState",
        "--property=SubState",
        "--property=MainPID",
    ])
    .await?;
    let text = std::str::from_utf8(&output.stdout)
        .context("systemctl --user show returned invalid UTF-8.")?;
    let properties = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect::<std::collections::BTreeMap<_, _>>();

    if properties.get("LoadState") == Some(&"not-found") {
        bail!(
            "{SERVICE_UNIT} is not installed. Run sh install.sh from the semmem checkout, then run semmem start."
        );
    }

    ensure_success(&output, "show semmem.service")?;
    let load_state = properties
        .get("LoadState")
        .context("systemctl --user show did not return LoadState.")?;
    let active_state = properties
        .get("ActiveState")
        .context("systemctl --user show did not return ActiveState.")?;
    let sub_state = properties
        .get("SubState")
        .context("systemctl --user show did not return SubState.")?;
    let main_pid = properties
        .get("MainPID")
        .context("systemctl --user show did not return MainPID.")?
        .parse::<u32>()
        .context("systemctl --user show returned an invalid MainPID.")?;

    Ok(json!({
        "unit": SERVICE_UNIT,
        "load_state": load_state,
        "active_state": active_state,
        "sub_state": sub_state,
        "main_pid": main_pid,
        "is_active": *active_state == "active",
    }))
}
