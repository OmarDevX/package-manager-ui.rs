use clap::{Arg, Command};
use reqwest::Client;
use serde::Deserialize;
use std::error::Error;
use std::fs;
use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};
use tar::Archive;
use flate2::read::GzDecoder;
use reqwest::header::CONTENT_TYPE;
use tokio::runtime::Runtime;
use eframe::egui;

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    description: String,
    urlpath: String,
}

#[derive(Default)]
struct AppState {
    log: Vec<String>,
    package_name: String,
    is_running: bool,
    progress: Option<String>,
    error: Option<String>,
    search_results: Vec<String>,
    selected_package: Option<String>,
}

impl AppState {
    fn log(&mut self, message: &str) {
        self.log.push(message.to_string());
    }

    fn clear_log(&mut self) {
        self.log.clear();
    }

    fn add_search_results(&mut self, results: Vec<String>) {
        self.search_results = results;
    }

    fn select_package(&mut self, package: Option<String>) {
        self.selected_package = package;
    }
}

struct MyApp {
    state: Arc<Mutex<AppState>>,
    rt: Runtime,
}
impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Lock state for mutable access
        let mut state = self.state.lock().unwrap();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Rust AUR Helper");

            // Input for package name
            ui.horizontal(|ui| {
                ui.label("Package:");
                ui.text_edit_singleline(&mut state.package_name);
            });

            // Search button
            if ui.button("Search").clicked() {
                let package_name = state.package_name.clone();
                if !package_name.is_empty() && !state.is_running {
                    state.is_running = true;
                    state.error = None;
                    state.progress = Some("Searching...".to_string());
                    
                    let state_clone = Arc::clone(&self.state);

                    self.rt.spawn(async move {
                        match search_aur_package(&package_name).await {
                            Ok(results) => {
                                let mut state = state_clone.lock().unwrap();
                                state.add_search_results(results);
                                state.is_running = false;
                                state.progress = None;
                                state.log.push("Search completed.".to_string());
                            }
                            Err(e) => {
                                let mut state = state_clone.lock().unwrap();
                                state.error = Some(e.to_string());
                                state.is_running = false;
                                state.log.push(format!("Search failed: {}", e));
                            }
                        }
                    });
                }
            }

            // Immutable borrow for search results
            let search_results = state.search_results.clone();
            let selected_package = state.selected_package.clone();
            drop(state); // End the immutable borrow

            // Display search results and handle selection
            for result in search_results {
                let mut state = self.state.lock().unwrap(); // Mutable borrow
                if ui.radio(selected_package.as_deref() == Some(&result), &result).clicked() {
                    state.select_package(Some(result.clone()));

                    // Check if the selected package is installed
                    if is_package_installed(&result).unwrap_or(false) {
                        state.progress = Some("Package is already installed.".to_string());
                    } else {
                        state.progress = None;
                    }
                }
            }

            // Re-lock state after the previous borrow ends
            let mut state = self.state.lock().unwrap();

            // Install/Uninstall button
            if let Some(package) = &state.selected_package {
                if !state.is_running {
                    let button_text = if is_package_installed(package).unwrap_or(false) {
                        "Uninstall"
                    } else {
                        "Install"
                    };

                    if ui.button(button_text).clicked() {
                        let package_clone = package.clone();
                        state.is_running = true;
                        state.error = None;
                        state.progress = Some(format!("{}...", button_text).to_string());

                        let state_clone = Arc::clone(&self.state);

                        self.rt.spawn(async move {
                            let result = if button_text == "Uninstall" {
                                uninstall_package(&package_clone)
                            } else {
                                run_package_management_logic(&package_clone, &state_clone).await
                            };

                            let mut state = state_clone.lock().unwrap();
                            if let Err(e) = result {
                                state.error = Some(e.to_string());
                                state.is_running = false;
                                state.log.push(format!("{} failed: {}", button_text, e));
                            } else {
                                state.progress = Some(format!("Package {} successfully.", button_text).to_string());
                                state.is_running = false;
                                state.log.push(format!("Package {} process completed.", button_text));
                            }
                        });
                    }
                }
            }

            // Display progress or error
            if let Some(error) = &state.error {
                ui.colored_label(egui::Color32::RED, error);
            }

            if let Some(progress) = &state.progress {
                ui.label(progress);
            }

            // Spinner if running
            if state.is_running {
                ui.spinner();
            } else {
                if ui.button("Clear Log").clicked() {
                    state.clear_log();
                }

                ui.group(|ui| {
                    ui.label("Log:");
                    for log in &state.log {
                        ui.label(log);
                    }
                });
            }
        });
    }
}



async fn search_aur_package(package_name: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let url = format!("https://aur.archlinux.org/rpc/?v=5&type=search&arg={}", package_name);
    let response = reqwest::get(&url).await?.json::<serde_json::Value>().await?;
    
    let packages = response["results"].as_array().unwrap_or(&vec![]).iter()
        .map(|pkg| pkg["Name"].as_str().unwrap_or("").to_string())
        .collect::<Vec<String>>();
    
    Ok(packages)
}

async fn fetch_metadata(package_name: &str) -> Result<Package, Box<dyn Error>> {
    let client = Client::new();
    let url = format!("https://aur.archlinux.org/rpc/?v=5&type=info&arg={}", package_name);
    println!("Fetching metadata from URL: {}", url);

    let response = client.get(&url).send().await?;
    
    let content_type = response.headers().get(CONTENT_TYPE)
        .ok_or("Missing content-type header")?
        .to_str()?;
    if !content_type.contains("application/json") {
        return Err("Unexpected content type".into());
    }

    let body = response.text().await?;
    println!("Response body: {}", body);

    let json_response = serde_json::from_str::<serde_json::Value>(&body)?;

    let package = json_response["results"].as_array().unwrap_or(&vec![]).iter().find_map(|pkg| {
        Some(Package {
            name: pkg["Name"].as_str().unwrap_or("").to_string(),
            version: pkg["Version"].as_str().unwrap_or("").to_string(),
            description: pkg["Description"].as_str().unwrap_or("").to_string(),
            urlpath: pkg["URLPath"].as_str().unwrap_or("").to_string(),
        })
    }).ok_or("Package not found")?;

    Ok(package)
}

async fn download_and_extract_package(urlpath: &str, dest: &str) -> Result<(), Box<dyn Error>> {
    let client = Client::new();
    let url = format!("https://aur.archlinux.org{}", urlpath);
    println!("Downloading package from URL: {}", url);

    let response = client.get(&url).send().await?;
    let content_type = response.headers().get(CONTENT_TYPE)
        .ok_or("Missing content-type header")?
        .to_str()?;
    if !content_type.contains("application/x-gzip") {
        return Err("Unexpected content type".into());
    }

    // Collect the response bytes into a `Vec<u8>`.
    let bytes = response.bytes().await?.to_vec();
    println!("Downloaded {} bytes", bytes.len());

    // Use the collected bytes to create the `GzDecoder`.
    let tarball = GzDecoder::new(&*bytes);
    let mut archive = Archive::new(tarball);

    // Create destination directory if it doesn't exist
    fs::create_dir_all(dest)?;

    // Unpack the archive
    println!("Extracting files to {}", dest);
    archive.unpack(dest)?;

    // Debug information
    println!("Files in {}:", dest);
    for entry in fs::read_dir(dest)? {
        let entry = entry?;
        let path = entry.path();
        println!("{}", path.display());
    }

    Ok(())
}

fn build_package(path: &str) -> Result<(), Box<dyn Error>> {
    // Ensure the correct path where PKGBUILD is located
    let build_dir = format!("{}/yay", path);
    println!("Building package in directory: {}", build_dir);

    let output = StdCommand::new("makepkg")
        .args(&["-si", "--noconfirm"])
        .current_dir(&build_dir)
        .output()?;
    if !output.status.success() {
        eprintln!("Failed to build package: {}", String::from_utf8_lossy(&output.stderr));
    } else {
        println!("Package built successfully.");
    }
    Ok(())
}
fn is_package_installed(package_name: &str) -> Result<bool, Box<dyn Error>> {
    let output = StdCommand::new("pacman")
        .args(&["-Q", package_name])
        .output()?;
    Ok(output.status.success())
}

fn install_package(package_file: &str) -> Result<(), Box<dyn Error>> {
    println!("Installing package from file: {}", package_file);
    let output = StdCommand::new("pkexec")
        .args(&["pacman", "-U", package_file, "--noconfirm"])
        .output()?;
    if !output.status.success() {
        eprintln!("Failed to install package: {}", String::from_utf8_lossy(&output.stderr));
    } else {
        println!("Package installed successfully.");
    }
    Ok(())
}
fn uninstall_package(package_name: &str) -> Result<(), Box<dyn Error>> {
    println!("Uninstalling package: {}", package_name);
    let output = StdCommand::new("pkexec")
        .args(&["pacman", "-Rns", package_name, "--noconfirm"])
        .output()?;
    if !output.status.success() {
        eprintln!("Failed to uninstall package: {}", String::from_utf8_lossy(&output.stderr));
    } else {
        println!("Package uninstalled successfully.");
    }
    Ok(())
}

fn find_package_file(base_directory: &str, package_name: &str) -> Option<String> {
    // Construct the path where the package file should be located
    let package_directory = format!("{}/{}", base_directory, package_name);

    // Check the directory for package files
    let entries = fs::read_dir(package_directory).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_file() {
            let file_name = path.file_name()?.to_string_lossy().to_string();
            if file_name.starts_with(package_name) && file_name.ends_with(".pkg.tar.zst") {
                return Some(path.to_string_lossy().to_string());
            }
        }
    }
    
    None
}
fn list_package_dependencies(package_name: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let output = StdCommand::new("pacman")
        .args(&["-Qi", package_name])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut dependencies = Vec::new();

    for line in stdout.lines() {
        if line.starts_with("Depends On") {
            dependencies.push(line.split(':').nth(1).unwrap_or("").trim().to_string());
        }
    }
    Ok(dependencies)
}

async fn run_package_management_logic(package_name: &str, state: &Arc<Mutex<AppState>>) -> Result<(), Box<dyn std::error::Error>> {
    let package = fetch_metadata(package_name).await?;

    let clone_path = format!("/tmp/{}", package.name);
    let download_result = download_and_extract_package(&package.urlpath, &clone_path).await;
    {
        let mut state = state.lock().unwrap();
        if let Err(e) = download_result {
            state.error = Some(e.to_string());
            state.is_running = false;
            return Ok(());
        }
        state.progress = Some("Package downloaded and extracted.".to_string());
    }

    let build_result = build_package(&clone_path);
    {
        let mut state = state.lock().unwrap();
        if let Err(e) = build_result {
            state.error = Some(e.to_string());
            state.is_running = false;
            return Ok(());
        }
        state.progress = Some("Package built successfully.".to_string());
    }

    // Use the correct directory and package name to find the package file
    let package_file = find_package_file("/tmp/yay", &package.name).ok_or("Package file not found")?;
    let install_result = install_package(&package_file);
    {
        let mut state = state.lock().unwrap();
        if let Err(e) = install_result {
            state.error = Some(e.to_string());
            state.is_running = false;
            return Ok(());
        }
        state.progress = Some("Package installed successfully.".to_string());
        state.is_running = false;
        state.log.push("Package installation process completed.".to_string());
    }

    Ok(())
}


fn run_cli() {
    let matches = Command::new("AUR Helper")
        .version("1.0")
        .author("Author Name <author@example.com>")
        .about("Helps manage AUR packages")
        .arg(Arg::new("package")
            .short('p')
            .long("package")
            .value_name("PACKAGE")
            .help("Specifies the package name"))
        .get_matches();

    if let Some(package) = matches.get_one::<String>("package") {
        let rt = Runtime::new().unwrap();
        let state = Arc::new(Mutex::new(AppState::default()));
        rt.block_on(async {
            let state_clone = state.clone();
            let result = run_package_management_logic(package, &state_clone).await;
            if let Err(e) = result {
                eprintln!("Error: {}", e);
            }
        });
    }
}

fn run_gui() {
    let state = Arc::new(Mutex::new(AppState::default()));
    let rt = Runtime::new().unwrap();
    let _ = eframe::run_native(
        "Rust AUR Helper GUI",
        eframe::NativeOptions {
            ..Default::default()
        },
        Box::new(move |cc| {
            Ok(Box::new(MyApp {
                state: state.clone(),
                rt: rt,
            }))
        }),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        run_cli();
    } else {
        run_gui();
    }
}
