//! CLI management for local device labels. Aliases point to canonical rows in
//! the store; they never become device identities.

use std::process::ExitCode;

use zestroku_core::store::Store;

use crate::store_db;

pub fn run(args: &[String]) -> ExitCode {
    if args.is_empty() {
        eprintln!("usage: zestroku alias <set|rm|ls> [<alias> [<device>]]");
        return ExitCode::from(2);
    }
    let store = match store_db::open() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("zestroku alias: store unavailable: {error}");
            return ExitCode::FAILURE;
        }
    };

    match args[0].as_str() {
        "set" if args.len() == 3 => {
            let alias = args[1].trim();
            if alias.is_empty() {
                eprintln!("zestroku alias: alias must not be blank");
                return ExitCode::from(2);
            }
            let device_id = &args[2];
            match store.get_device(device_id) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    eprintln!("zestroku alias: unknown device {device_id:?}");
                    return ExitCode::from(2);
                }
                Err(error) => {
                    eprintln!("zestroku alias: cannot read device: {error}");
                    return ExitCode::FAILURE;
                }
            }
            let row = zestroku_core::store::AliasRow {
                alias: alias.to_owned(),
                device_id: device_id.to_owned(),
            };
            match store.set_alias(&row) {
                Ok(()) => {
                    println!("{alias} -> {device_id}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("zestroku alias: cannot save alias: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        "rm" if args.len() == 2 => {
            let alias = args[1].trim();
            let exists = match store.list_aliases() {
                Ok(aliases) => aliases.iter().any(|row| row.alias == alias),
                Err(error) => {
                    eprintln!("zestroku alias: cannot list aliases: {error}");
                    return ExitCode::FAILURE;
                }
            };
            if !exists {
                eprintln!("zestroku alias: no alias named {alias:?}");
                return ExitCode::from(2);
            }
            match store.delete_alias(alias) {
                Ok(()) => {
                    println!("removed alias {alias}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("zestroku alias: cannot remove alias: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        "ls" if args.len() == 1 => match store.list_aliases() {
            Ok(aliases) if aliases.is_empty() => {
                println!("no aliases");
                ExitCode::SUCCESS
            }
            Ok(aliases) => {
                for row in aliases {
                    println!("{} -> {}", row.alias, row.device_id);
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("zestroku alias: cannot list aliases: {error}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: zestroku alias <set|rm|ls> [<alias> [<device>]]");
            ExitCode::from(2)
        }
    }
}
