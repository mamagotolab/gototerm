use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(
        args.first().map(String::as_str),
        Some("init-hooks" | "remove-hooks")
    ) {
        let project = args
            .get(1)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        let agent = args.get(2).map(String::as_str).unwrap_or("all");
        let agents = if agent == "all" {
            vec!["claude", "codex"]
        } else {
            vec![agent]
        };
        let result = std::env::current_exe()
            .map_err(|error| error.to_string())
            .and_then(|exe| {
                gototerm::agent_hooks::setup(&project, &exe, &agents, args[0] == "remove-hooks")
            });
        match result {
            Ok(()) => println!("フック設定を更新しました。Codexは /hooks で追加したフックを確認・信頼してください。"),
            Err(error) => { eprintln!("{error}"); std::process::exit(1); }
        }
        return;
    }
    let Some(agent) = args.first().map(String::as_str) else {
        return;
    };
    let mut bytes = Vec::new();
    if std::io::stdin()
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return;
    }
    if let Some(event) = gototerm::agent_hooks::event_from_hook(agent, &bytes) {
        #[cfg(windows)]
        send(&event);
        #[cfg(not(windows))]
        let _ = event;
    }
    // Empty JSON carries no permission decision, context, or continue/block directive.
    if agent == "codex" {
        let _ = std::io::stdout().write_all(b"{}\n");
    }
}

#[cfg(windows)]
fn send(event: &[u8]) {
    let Ok(pipe) = std::env::var("GOTOTERM_STATE_PIPE") else {
        return;
    };
    if !pipe.starts_with(r"\\.\pipe\gototerm-state-") {
        return;
    }
    for _ in 0..20 {
        if let Ok(mut stream) = std::fs::OpenOptions::new().write(true).open(&pipe) {
            let _ = stream.write_all(event);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
