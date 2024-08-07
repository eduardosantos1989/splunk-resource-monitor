use std::{fs::{self, OpenOptions}, io::BufWriter, net::{ToSocketAddrs, UdpSocket}, path::Path, process};
use std::io::Write;
use gethostname::gethostname;
mod modules {
    pub mod startup;
    pub mod config;
    pub mod log_entry;
}
use sysinfo::{System, Networks, Pid};


fn check_running_process(exe: &Path, current_pid: &u32) {
    let pid_file_path = exe.join(".agent.pid");
    let executable = std::env::current_exe().unwrap();
    let pid_file = pid_file_path.to_str().unwrap();

    let mut system = System::new_all();
    system.refresh_all();

    if !Path::new(pid_file).exists() {
        // Create the PID file and write the appropriate PID
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(pid_file)
            .expect("Failed to create or open PID file");

        let mut matching_processes = Vec::new();
        for (pid, process) in system.processes() {
            if process.pid() != Pid::from_u32(*current_pid)
                && process.parent().map_or(true, |parent| parent != Pid::from_u32(*current_pid))
                && process.exe().map_or(false, |exe| exe == executable)
                && process.cmd().get(1).map_or(false, |arg| arg == "agent")
            {
                matching_processes.push((pid.as_u32(), process.start_time()));
            }
            
        }

        // Find the oldest process
        if let Some((oldest_pid, _)) = matching_processes.into_iter()
            .min_by_key(|&(pid, start_time)| (start_time, pid)) 
        {
            println!("Found running process with PID: {}", oldest_pid);
            writeln!(file, "{}", oldest_pid).expect("Failed to write to PID file");
            process::exit(0);
        } else {
            writeln!(file, "{}", *current_pid).expect("Failed to write to PID file");
        }

    } else {
        let content = fs::read_to_string(pid_file).expect("Failed to read PID file");
        let old_pid = match content.trim().parse::<u32>() {
            Ok(pid) => pid,
            Err(_) => {
                fs::remove_file(pid_file).expect("Failed to delete invalid PID file");
                process::exit(1);
            }
        };

        if system.process(Pid::from_u32(old_pid)).is_some() {
            println!("Process with PID {} is running", old_pid);
            process::exit(0);
        } else {
            println!("Process with PID {} is not running", old_pid);
            let mut file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(pid_file)
                .expect("Failed to open PID file");
            let current_pid = process::id();
            writeln!(file, "{}", current_pid).expect("Failed to write to PID file");
            println!("PID {} is written to file.", current_pid);
        }
    }
}   



fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.is_empty() {
        println!("No arguments provided");
        process::exit(1);
    }

    let configmap = modules::config::get_configmap(&args[1]);
    let hostname = gethostname().to_string_lossy().to_string();

    if args[1] == "startup" {
        let startup_entry = modules::startup::startup_log(hostname, &configmap.root_folder, &configmap.app_folder);
        let startup_path = configmap.log_folder;
        let startup_file = startup_path.join("startup_json.log");

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(startup_file)
            .expect("Failed to open PID file");
        writeln!(file, "{}", startup_entry.unwrap()).expect("Failed to write to PID file");
        process::exit(0);
    } else if args[1] == "agent" {
        let current_pid = process::id();
        check_running_process(&configmap.bin_folder, &current_pid);
    }
    
    
    let component = String::from("agent");
    let mut sys = System::new_all();
    let mut networks = Networks::new_with_refreshed_list();
    let interval = configmap.interval;
    let agent_path = configmap.log_folder;
    let agent_file = agent_path.join("hostagent_json.log");

    let agent_uptime = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .expect("Time went backwards")
    .as_secs()-20;

    let mut log_entry = modules::log_entry::LogEntry::new(hostname.clone(), component.clone(), agent_uptime);

    if configmap.log_type == "file" {
        let mut log_writer = BufWriter::new(OpenOptions::new()
            .create(true)
        .append(true)
        .open(agent_file.clone())
        .expect("Failed to open log file"));

        loop {
            log_entry.timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs();
            log_entry.uptime = System::uptime();
    
            for _ in 0..interval {
                sys.refresh_all();
                networks.refresh();
                log_entry.update(&sys, &networks);
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            
            log_entry.finalize(interval);
    
            log_entry.write_json(&mut log_writer).expect("Failed to write to log file");

            log_writer.write_all(b"\n").expect("Failed to write newline");
            log_writer.flush().expect("Failed to flush log file");
            log_entry.reset();
    
            modules::startup::check_stopswitch(&configmap.bin_folder);
            modules::log_entry::check_log_file_size(agent_file.as_ref());
        }
    } else if configmap.log_type == "udp" {
        let udp_host = configmap.host.clone();
        let udp_port = configmap.port;
        let socket = UdpSocket::bind("0.0.0.0:0").expect("Couldn't bind to address");
    
        loop {
            log_entry.timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs();
            log_entry.uptime = System::uptime();
    
            for _ in 0..interval {
                sys.refresh_all();
                networks.refresh();
                log_entry.update(&sys, &networks);
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
    
            log_entry.finalize(interval);
    
            let mut json_buffer = Vec::new();
            log_entry.write_json(&mut json_buffer).expect("Failed to serialize log entry");
            let json_string = String::from_utf8(json_buffer).expect("Failed to convert JSON buffer to string");
    
            // Resolve the hostname to an IP address before each send
            let udp_address = format!("{}:{}", udp_host, udp_port);
            let resolved_address = udp_address.to_socket_addrs()
                .expect("Failed to resolve hostname")
                .next()
                .expect("No addresses found for hostname");
    
            socket.send_to(json_string.as_bytes(), resolved_address).expect("Failed to send UDP message");
            log_entry.reset();
            modules::startup::check_stopswitch(&configmap.bin_folder);
        }
    }

}
