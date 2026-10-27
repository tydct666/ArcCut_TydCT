use arkcut_core::Engine;
use serde_json::json;
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("arkcut-cli request.json | inline JSON; uses same core as desktop");
        std::process::exit(2)
    }
    let text = if args[1].starts_with('{') {
        args[1].clone()
    } else {
        std::fs::read_to_string(&args[1]).expect("request file")
    };
    let v: serde_json::Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).expect("request JSON");
    let engine = Engine::default();
    match engine.request(v["op"].as_str().unwrap_or(""), v["payload"].clone()) {
        Ok(start) => {
            if start["jobId"].is_string() {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let state = engine.request("status", json!({"after":0})).unwrap();
                    let job = &state["job"];
                    if job["status"] != "running" && job["status"] != "cancelling" {
                        println!("{}", serde_json::to_string_pretty(&state).unwrap());
                        if job["status"] != "completed" {
                            std::process::exit(1)
                        }
                        break;
                    }
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&start).unwrap());
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}
