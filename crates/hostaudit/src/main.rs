//! hostaudit — configuration / operations security audit (canon:
//! docs/hostaudit-baseline-v1.md; testable interface: crates/hostaudit/CONTRACT.md).
//!
//! Exit codes (CONTRACT §2): 0 all clean / not_applicable (or success),
//! 1 at least one violation / undetermined, 2 usage error, 3 refused,
//! 4 inventory undetermined.

mod apply;
mod aws;
mod baseline;
mod config;
mod finding;
mod inventory;
mod local;
mod osv;
mod redact;
mod session;
mod ssh;
mod tls;

use clap::{Parser, Subcommand};
use finding::{print_err, print_out};

#[derive(Parser)]
#[command(
    name = "hostaudit",
    version,
    about = "Configuration / operations security audit (read-only; SG changes only on explicit confirmation)"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List audit targets (ssh config, --from-cmd, EC2).
    Inventory {
        #[arg(long)]
        json: bool,
        /// Shell command whose stdout lines are extra hosts (only exit 0 counts).
        #[arg(long = "from-cmd", allow_hyphen_values = true)]
        from_cmd: Option<String>,
        /// Include EC2 instances of the logged-in AWS account.
        #[arg(long)]
        aws: bool,
    },
    /// Choose which hosts may be connected to (`--all` allowed).
    Select {
        #[arg(long = "from-cmd", allow_hyphen_values = true)]
        from_cmd: Option<String>,
        #[arg(long)]
        aws: bool,
        #[arg(long, conflicts_with = "aliases")]
        all: bool,
        aliases: Vec<String>,
    },
    /// Read-only ssh audit of the selected hosts against baseline v1.
    Audit {
        #[arg(long)]
        json: bool,
    },
    /// Audit local Claude Code settings.
    Local {
        #[arg(long)]
        json: bool,
    },
    /// Read-only AWS audit; `aws apply` changes one security-group rule on confirmation.
    Aws {
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        sub: Option<AwsCmd>,
    },
    /// Print the ~/.daily/config.toml stanza.
    DailyTask {
        #[arg(long)]
        print: bool,
    },
    /// SessionStart hook: one-line systemMessage with the latest counts.
    #[command(hide = true)]
    SessionStart,
}

#[derive(Subcommand)]
enum AwsCmd {
    /// Revoke (optionally replace) the ingress rule of one A1/A2 finding.
    Apply {
        #[arg(long)]
        finding: String,
        /// The sgr id of the finding, as explicit confirmation.
        #[arg(long)]
        confirm: Option<String>,
        /// IPv4 CIDR for a replacement rule (required when closing the last ssh rule).
        #[arg(long = "replace-cidr")]
        replace_cidr: Option<String>,
    },
}

fn usage(msg: &str) -> i32 {
    print_err(&format!("hostaudit: {msg}"));
    2
}

fn run(cli: Cli) -> i32 {
    redact::register_aws_env();
    if let Cmd::DailyTask { print } = &cli.command {
        if !*print {
            return usage("daily-task: use `hostaudit daily-task --print`");
        }
        print_out(&session::daily_stanza());
        return 0;
    }
    let ctx = match config::load() {
        Ok(c) => c,
        Err(e) => {
            if matches!(cli.command, Cmd::SessionStart) {
                // The user-visible line must still appear, saying it is unknown.
                let msg = format!("hostaudit: configuration error — state UNKNOWN ({e})");
                print_out(&serde_json::json!({ "systemMessage": msg }).to_string());
                return 0;
            }
            return usage(&e);
        }
    };
    match cli.command {
        Cmd::DailyTask { .. } => 0,
        Cmd::SessionStart => {
            let msg = session::session_message(&ctx);
            print_out(&serde_json::json!({ "systemMessage": msg }).to_string());
            0
        }
        Cmd::Inventory {
            json,
            from_cmd,
            aws,
        } => match inventory::build(&ctx, from_cmd.as_deref(), aws) {
            Err(e) => {
                print_err(&format!(
                    "hostaudit inventory: UNDETERMINED — {e}; no host list is printed"
                ));
                4
            }
            Ok(inv) => {
                if json {
                    let hosts: Vec<serde_json::Value> =
                        inv.hosts.iter().map(inventory::Host::to_json).collect();
                    print_out(
                        &serde_json::json!({
                            "ssh_config": if inv.ssh_config_present { "present" } else { "absent" },
                            "hosts": hosts,
                        })
                        .to_string(),
                    );
                } else {
                    let mut s = format!(
                        "~/.ssh/config: {}\n",
                        if inv.ssh_config_present {
                            "present"
                        } else {
                            "absent"
                        }
                    );
                    for h in &inv.hosts {
                        s.push_str(&format!(
                            "{:<24} {:<10} {}\n",
                            h.alias,
                            format!("{:?}", h.source),
                            h.hostname.as_deref().unwrap_or("-")
                        ));
                    }
                    s.push_str(&format!("{} hosts\n", inv.hosts.len()));
                    print_out(&s);
                }
                0
            }
        },
        Cmd::Select {
            from_cmd,
            aws,
            all,
            aliases,
        } => {
            if !all && aliases.is_empty() {
                return usage("select: give one or more aliases, or --all");
            }
            let inv = match inventory::build(&ctx, from_cmd.as_deref(), aws) {
                Ok(i) => i,
                Err(e) => {
                    print_err(&format!(
                        "hostaudit select: inventory UNDETERMINED — {e}; selection unchanged"
                    ));
                    return 4;
                }
            };
            let chosen: Vec<inventory::Host> = if all {
                inv.hosts.clone()
            } else {
                let unknown: Vec<&String> = aliases
                    .iter()
                    .filter(|a| !inv.hosts.iter().any(|h| &h.alias == *a))
                    .collect();
                if !unknown.is_empty() {
                    print_err(&format!(
                        "hostaudit select: REFUSED — not in the inventory: {}; selection unchanged",
                        unknown
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    return 3;
                }
                inv.hosts
                    .iter()
                    .filter(|h| aliases.contains(&h.alias))
                    .cloned()
                    .collect()
            };
            if chosen.is_empty() {
                print_err("hostaudit select: REFUSED — the inventory is empty, nothing to select; selection unchanged");
                return 3;
            }
            match inventory::write_selection(
                &ctx,
                &inventory::Selection {
                    host: chosen.clone(),
                },
            ) {
                Ok(()) => {
                    print_out(&format!(
                        "selected {} host(s): {}",
                        chosen.len(),
                        chosen
                            .iter()
                            .map(|h| h.alias.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    0
                }
                Err(e) => {
                    print_err(&format!("hostaudit select: selection NOT written: {e}"));
                    1
                }
            }
        }
        Cmd::Audit { json } => baseline::run_audit(&ctx, json),
        Cmd::Local { json } => local::run_local(&ctx, json),
        Cmd::Aws { json, sub } => match sub {
            None => aws::run_aws(&ctx, json),
            Some(AwsCmd::Apply {
                finding,
                confirm,
                replace_cidr,
            }) => apply::run(&ctx, &finding, confirm.as_deref(), replace_cidr.as_deref()),
        },
    }
}

fn main() {
    let cli = Cli::parse();
    std::process::exit(run(cli));
}
