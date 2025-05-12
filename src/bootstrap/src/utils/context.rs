#![allow(dead_code)]
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::Mutex;
use std::path::Path;

use std::collections::HashMap;
use std::path::PathBuf;
use build_helper::git::PathFreshness;

use crate::CommandOutput;

use super::cache::Interned;
use super::exec::{BehaviorOnFailure, BootstrapCommand, OutputMode};


pub struct ExecutionContext {
    dry_run: bool,
    verbose: usize,
    fail_fast: bool,

    command_output_cache: Mutex<HashMap<(PathBuf, Vec<Vec<u8>>, Option<PathBuf>), Result<CommandOutput, String>>>,
    file_contents_cache: Mutex<HashMap<PathBuf, std::io::Result<String>>>,
    path_exist_cache: Mutex<HashMap<PathBuf, bool>>,
    path_modifications_cache: Mutex<HashMap<(PathBuf, Interned<String>), PathFreshness>>
}

impl ExecutionContext {
    pub fn new(dry_run: bool, verbose: usize, fail_fast: bool) -> Self {
        Self {
            dry_run,
            verbose,
            fail_fast,
            command_output_cache: Mutex::new(HashMap::new()),
            file_contents_cache: Mutex::new(HashMap::new()),
            path_exist_cache: Mutex::new(HashMap::new()),
            path_modifications_cache: Mutex::new(HashMap::new())
        }
    }


    fn execute_bootstrap_command_internal(&self, cmd: &mut BootstrapCommand, stdout_mode: OutputMode, stderr_mode: OutputMode) -> Result<CommandOutput, String> {
        if self.dry_run && !cmd.run_always {
            self.verbose_print(&format!("(dry run) {:?}", cmd));
            cmd.mark_as_executed();
            return Ok(CommandOutput::default())
        }

        self.verbose_print(&format!("running: {:?}", cmd));

        let command = cmd.as_command_mut();
        command.stdout(stdout_mode.stdio());
        command.stderr(stderr_mode.stdio());

        let output = match command.output() {
            Ok(output) => {
                self.verbose_print(&format!("finished running {:?}", command));
                CommandOutput::from_output(output, stdout_mode, stderr_mode)
            }
            Err(e) => {
                let error_msg = format!("failed to execute {:?}: {}", command, e);
                self.verbose_print(&error_msg);
                let output = CommandOutput::did_not_start(stdout_mode, stderr_mode);
                self.handle_failure(cmd, &output, &error_msg);
                return Err(error_msg);
            }
        };

        cmd.mark_as_executed();

        if output.is_failure() &&  cmd.failure_behavior != BehaviorOnFailure::Ignore {
            let error_msg = format!("command failed: {:?}", cmd);
            self.handle_failure(cmd, &output, &error_msg);
            Err(error_msg)
        } else {
            Ok(output)
        }
    }


    fn handle_failure(&self, cmd: &BootstrapCommand, output: &CommandOutput, error_msg: &str) {
        if let Some(stderr) = output.stderr_if_present() {
            eprintln!("{}\nStderr:\n{}", error_msg, stderr);
        } else {
            eprintln!("{}", error_msg);
        }

        match cmd.failure_behavior {
            BehaviorOnFailure::Exit => {
                if self.fail_fast {
                    self.fatal_error(&format!("Exiting due to command failure: {:?}", cmd));
                } else {
                    eprintln!("(Failure Delayed)");
                }
            }
            BehaviorOnFailure::DelayFail => {
                eprintln!("(Failure delayed)");
            }
            BehaviorOnFailure::Ignore => {}
        }

    }


    pub fn read_file(&mut self, path: &Path) -> String {
        let mut cache = self.file_contents_cache.lock().unwrap();
        if let Some(cached_result) = cache.get(path) {
            self.verbose_print(&format!("(cached) Reading file: {:?}", path.display()));
            return cached_result.as_ref().expect("Should be present").clone().to_owned();
        }
        self.verbose_print(&format!("Reading file: {}", path.display()));
        let result = std::fs::read_to_string(path);
        let value = result.as_ref().expect("Should be present").to_owned();
        cache.insert(path.to_path_buf(), result);
        value
    }

    pub fn path_exists(&mut self, path: &Path) -> bool {
        let mut cache = self.path_exist_cache.lock().unwrap();
        if let Some(cached_result) = cache.get(path) {
            self.verbose_print(&format!("(cached) Checking path existence: {}", path.display()));
            return *cached_result;
        }

        self.verbose_print(&format!("Checking path existence: {}", path.display()));
        let result = path.exists();
        cache.insert(path.to_path_buf(), result);
        result
    }


    pub fn git_command_output(&mut self, cwd: Option<&Path>, args: &[&OsStr]) -> Result<String, String> {
        let program = Path::new("git");
        let mut cmd = BootstrapCommand::new(program);
        if let Some(dir) = cwd { cmd.current_dir(dir); }
        cmd.args(args);
        cmd.allow_failure().run_always();

        let command_key = {
            let mut key_program = PathBuf::from(program);
            let key_args: Vec<Vec<u8>> = args.iter().map(|a| a.as_bytes().to_vec()).collect();
            let key_cwd = cwd.map(|p| p.to_path_buf());
            (key_program, key_args, key_cwd)
        };

        let mut cache = self.command_output_cache.lock().unwrap();
        if let Some(cached_result) = cache.get(&command_key) {
            self.verbose_print(&format!("(cached) Running git command: {:?}", cmd));
            return cached_result.as_ref().and_then(|output| output.stdout_if_present()).and_then(|| "Cached command failed".to_string());
        }

        let result = self.execute_bootstrap_command_internal(&mut cmd, OutputMode::Capture, OutputMode::Capture);

        cache.insert(command_key.clone(), result.clone());

        result.and_then(|output| output.stdout_if_present().ok_or_else(|| "Command succeeded but stdout not captured or invalid UTF-8".to_string()))
           .map_err(|e| format!("Git command failed: {}", e)) 

    }

    pub fn fatal_error(&self, msg: &str) {
        eprintln!("fatal error: {}", msg);
        std::process::exit(1);
    }

    pub fn verbose_print(&self, msg: &str) {
        if self.verbose > 0 {
            println!("{}", msg);
        }
    }
}