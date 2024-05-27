use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};
use std::fs;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};
use ws::{listen, CloseCode, Sender};
use lazy_static::lazy_static;
use std::sync::atomic::{AtomicBool, Ordering};

fn main() {
    // Build the site initially
    build_site().unwrap();

    // Set up the file watcher
    let (tx, rx) = mpsc::channel();
    let server_control = Arc::new(Mutex::new(true));
    let server_control_clone = Arc::clone(&server_control);

    thread::spawn(move || {
        watch_content_directory(tx);
    });

    // Start the web server and WebSocket server in separate threads
    let server_thread = thread::spawn(move || {
        start_server(server_control_clone);
    });

    let ws_thread = thread::spawn(move || {
        start_ws_server();
    });

    // Watch for file changes and rebuild the site
    loop {
        match rx.recv() {
            Ok(_) => {
                println!("Changes detected, rebuilding site...");
                build_site().unwrap();

                // Restart the server
                let mut control = server_control.lock().unwrap();
                *control = false;

                // Wait for the server to stop
                thread::sleep(Duration::from_secs(1));

                // Start a new server
                let server_control_clone = Arc::clone(&server_control);
                thread::spawn(move || {
                    start_server(server_control_clone);
                });

                *control = true;

                // Notify WebSocket clients to reload
                NOTIFY_RELOAD.store(true, Ordering::Relaxed);
            }
            Err(e) => println!("watch error: {:?}", e),
        }
    }

    // Join the server threads to keep the program running
    server_thread.join().unwrap();
    ws_thread.join().unwrap();
}

fn start_server(control: Arc<Mutex<bool>>) {
    let listener = TcpListener::bind("127.0.0.1:7878").unwrap();
    println!("Server listening on port 7878");

    for stream in listener.incoming() {
        let stream = stream.unwrap();
        handle_connection(stream);

        let control = control.lock().unwrap();
        if !*control {
            break;
        }
    }

    println!("Server stopped.");
}

fn start_ws_server() {
    listen("127.0.0.1:7879", |out| {
        WS_CLIENTS.lock().unwrap().push(out.clone());
        move |msg| {
            if NOTIFY_RELOAD.load(Ordering::Relaxed) {
                out.send("reload").unwrap();
                NOTIFY_RELOAD.store(false, Ordering::Relaxed);
            }
            Ok(())
        }
    }).unwrap();
}

fn handle_connection(mut stream: TcpStream) {
    let mut buffer = [0; 1024];
    stream.read(&mut buffer).unwrap();

    let get = b"GET / HTTP/1.1\r\n";
    let (status_line, filename) = if buffer.starts_with(get) {
        ("HTTP/1.1 200 OK\r\n\r\n", "output/pages/index.html".to_string())
    } else {
        // Extract the requested path from the request buffer
        let request = String::from_utf8_lossy(&buffer[..]);
        let path = request.lines().next().unwrap().split_whitespace().nth(1).unwrap();
        let filepath = format!("output{}", path);
        let file_path = Path::new(&filepath);

        if file_path.exists() {
            ("HTTP/1.1 200 OK\r\n\r\n", filepath)
        } else {
            ("HTTP/1.1 404 NOT FOUND\r\n\r\n", "output/pages/404.html".to_string())
        }
    };

    let mut contents = fs::read_to_string(filename).unwrap();

    // Inject JavaScript for auto reload
    contents.push_str(
        "<script>
            const ws = new WebSocket('ws://127.0.0.1:7879');
            ws.onmessage = (event) => {
                if (event.data === 'reload') {
                    location.reload();
                }
            };
        </script>"
    );

    let response = format!("{}{}", status_line, contents);

    stream.write(response.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn build_site() -> std::io::Result<()> {
    let collections = vec!["pages", "projects"];

    for collection in collections {
        let collection_path = format!("content/{}", collection);
        let output_path = format!("output/{}", collection);
        fs::create_dir_all(&output_path)?;

        for entry in fs::read_dir(collection_path)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                let filename = path.file_stem().unwrap().to_str().unwrap();
                let markdown_content = fs::read_to_string(&path)?;
                let html_content = markdown_to_html(&markdown_content);
                let output_file = format!("{}/{}.html", output_path, filename);
                let html = apply_template(&format!("{}.html", collection), &html_content);
                fs::write(output_file, html)?;
            }
        }
    }

    Ok(())
}

fn markdown_to_html(markdown: &str) -> String {
    let mut html = String::new();
    for line in markdown.lines() {
        if line.starts_with("# ") {
            html.push_str(&format!("<h1>{}</h1>\n", &line[2..]));
        } else if line.starts_with("## ") {
            html.push_str(&format!("<h2>{}</h2>\n", &line[3..]));
        } else if line.starts_with("### ") {
            html.push_str(&format!("<h3>{}</h3>\n", &line[4..]));
        } else if line.starts_with("#### ") {
            html.push_str(&format!("<h4>{}</h4>\n", &line[5..]));
        } else if line.starts_with("##### ") {
            html.push_str(&format!("<h5>{}</h5>\n", &line[6..]));
        } else if line.starts_with("###### ") {
            html.push_str(&format!("<h6>{}</h6>\n", &line[7..]));
        } else if line.starts_with("[") && line.contains("](") {
            let end_bracket = line.find(']').unwrap();
            let start_paren = line.find('(').unwrap();
            let end_paren = line.find(')').unwrap();
            let text = &line[1..end_bracket];
            let url = &line[start_paren + 1..end_paren];
            html.push_str(&format!("<a href=\"{}\">{}</a>\n", url, text));
        } else {
            html.push_str(&format!("<p>{}</p>\n", line));
        }
    }
    html
}

fn apply_template(template_name: &str, content: &str) -> String {
    let template_path = format!("templates/{}", template_name);
    let template = fs::read_to_string(template_path).unwrap();
    
    // Apply the collection template
    let collection_content = template.replace("{{ content }}", content);
    
    // Apply the base template
    let base_template = fs::read_to_string("templates/base.html").unwrap();
    base_template.replace("{{ content }}", &collection_content)
                 .replace("{{ title }}", "My Site")
}

fn watch_content_directory(tx: mpsc::Sender<()>) {
    let mut last_modified = SystemTime::now();

    loop {
        thread::sleep(Duration::from_secs(2));

        let mut changed = false;
        for entry in fs::read_dir("content").unwrap() {
            let entry = entry.unwrap();
            let metadata = fs::metadata(entry.path()).unwrap();
            let modified = metadata.modified().unwrap();

            if modified > last_modified {
                changed = true;
                last_modified = modified;
            }
        }

        if changed {
            tx.send(()).unwrap();
        }
    }
}

// Globals for WebSocket reload notification
lazy_static! {
    static ref WS_CLIENTS: Mutex<Vec<Sender>> = Mutex::new(Vec::new());
    static ref NOTIFY_RELOAD: AtomicBool = AtomicBool::new(false);
}

