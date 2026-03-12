use std::{
    collections::BTreeMap,
    env, fs,
    fs::File,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use cargo_metadata::{CrateType, Metadata, MetadataCommand, Package};
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use tar::Builder;
use tempfile::tempdir_in;

#[derive(Parser, Debug)]
#[command(name = "rustforge", about = "Engine mod development CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Status,
    Mod(ModCommand),
    Dev(DevArgs),
}

#[derive(Args, Debug)]
struct ModCommand {
    #[command(subcommand)]
    command: ModSubcommand,
}

#[derive(Subcommand, Debug)]
enum ModSubcommand {
    Build(ModBuildArgs),
    Package(ModPackageArgs),
}

#[derive(Args, Debug, Clone)]
struct ModBuildArgs {
    #[arg(long)]
    release: bool,
    #[arg(long)]
    target: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct ModPackageArgs {
    #[arg(long)]
    release: bool,
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    all_platforms: bool,
}

#[derive(Args, Debug)]
struct DevArgs {
    #[arg(long)]
    release_engine: bool,
    #[arg(long)]
    release_mod: bool,
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    hot: bool,
}

#[derive(Debug, Clone)]
struct App {
    project_root: PathBuf,
    manifest_path: PathBuf,
    metadata: Metadata,
    package: Package,
    config: RustforgeConfig,
    engine: EngineSpec,
}

#[derive(Debug, Clone)]
struct EngineSpec {
    repo_url: String,
    commit: String,
    commit_short: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RustforgeConfig {
    mod_id: Option<String>,
    #[serde(default)]
    engine: EngineConfig,
    #[serde(default)]
    paths: PathConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct EngineConfig {
    repo_url: Option<String>,
    bin: Option<String>,
    #[serde(default)]
    features: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PathConfig {
    engine_root: Option<String>,
    out_dir: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlatformKind {
    Windows,
    MacOs,
    Unix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FileStamp {
    modified_millis: u128,
    len: u64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let app = App::load()?;

    match cli.command {
        Commands::Status => status(&app),
        Commands::Mod(mod_command) => match mod_command.command {
            ModSubcommand::Build(args) => build_mod(&app, args.release, args.target.as_deref()),
            ModSubcommand::Package(args) => {
                let artifacts = package_mod_targets(
                    &app,
                    args.release,
                    args.target.as_deref(),
                    args.all_platforms,
                )?;
                for artifact in artifacts {
                    println!("{}", artifact.display());
                }
                Ok(())
            }
        },
        Commands::Dev(args) => dev(&app, &args),
    }
}

impl App {
    fn load() -> Result<Self> {
        let project_root =
            env::current_dir().context("failed to determine current working directory")?;
        Self::load_from(&project_root)
    }

    fn load_from(project_root: &Path) -> Result<Self> {
        let project_root = project_root.to_path_buf();
        let manifest_path = project_root.join("Cargo.toml");
        ensure!(
            manifest_path.exists(),
            "Cargo.toml not found in {}; run rustforge from the mod project root",
            project_root.display()
        );

        let metadata = MetadataCommand::new()
            .current_dir(&project_root)
            .no_deps()
            .exec()
            .context("failed to load cargo metadata for mod project")?;

        let package = metadata
            .root_package()
            .cloned()
            .ok_or_else(|| anyhow!("no root package found for current Cargo project"))?;

        let config = load_config(&manifest_path, &project_root)?;
        let engine = load_engine_spec(&project_root, &config)?;

        Ok(Self {
            project_root,
            manifest_path,
            metadata,
            package,
            config,
            engine,
        })
    }

    fn mod_id(&self) -> String {
        self.config
            .mod_id
            .clone()
            .unwrap_or_else(|| self.package.name.to_string())
    }

    fn engine_mirror_dir(&self) -> PathBuf {
        self.project_root.join(".rustforge/engine/mirror.git")
    }

    fn engine_worktree_dir(&self) -> PathBuf {
        resolve_configured_path(
            &self.project_root,
            self.config.paths.engine_root.as_deref(),
            &format!(".rustforge/engine/worktrees/{}", self.engine.commit),
            &self.engine.commit,
        )
    }

    fn out_dir(&self) -> PathBuf {
        resolve_configured_path(
            &self.project_root,
            self.config.paths.out_dir.as_deref(),
            ".rustforge/out",
            &self.engine.commit,
        )
    }

    fn dist_dir(&self) -> PathBuf {
        self.project_root.join("dist")
    }

    fn test_artifact_path(&self) -> PathBuf {
        self.out_dir().join(format!("{}.rf", self.mod_id()))
    }

    fn final_artifact_path(&self) -> PathBuf {
        let version = self.package.version.to_string();
        let mut filename = self.mod_id();
        if !version.is_empty() {
            filename.push('-');
            filename.push_str(&version);
        }
        filename.push_str("-engine-");
        filename.push_str(&self.engine.commit_short);
        filename.push_str(".rf");
        self.dist_dir().join(filename)
    }

    fn artifact_path_for(
        &self,
        release: bool,
        target: Option<&str>,
        include_target: bool,
    ) -> PathBuf {
        let mut base = self.mod_id();
        if release {
            let version = self.package.version.to_string();
            if !version.is_empty() {
                base.push('-');
                base.push_str(&version);
            }
        }
        if include_target {
            if let Some(target) = target {
                base.push('-');
                base.push_str(target);
            }
        }
        if release {
            base.push_str("-engine-");
            base.push_str(&self.engine.commit_short);
            base.push_str(".rf");
            self.dist_dir().join(base)
        } else {
            base.push_str(".rf");
            self.out_dir().join(base)
        }
    }

    fn mod_cdylib_target_name(&self) -> Result<String> {
        self.package
            .targets
            .iter()
            .find(|target| {
                target
                    .crate_types
                    .iter()
                    .any(|kind| *kind == CrateType::CDyLib)
            })
            .map(|target| target.name.replace('-', "_"))
            .ok_or_else(|| anyhow!("no cdylib target found in {}", self.manifest_path.display()))
    }
}

fn status(app: &App) -> Result<()> {
    println!("Project: {}", app.project_root.display());
    println!("Engine repo: {}", app.engine.repo_url);
    println!("Engine commit: {}", app.engine.commit);
    println!("Mirror: {}", exists_or_missing(app.engine_mirror_dir()));
    println!("Worktree: {}", exists_or_missing(app.engine_worktree_dir()));
    println!("Test artifact: {}", app.test_artifact_path().display());
    println!("Final artifact: {}", app.final_artifact_path().display());
    Ok(())
}

fn dev(app: &App, args: &DevArgs) -> Result<()> {
    sync_engine(app)?;

    let engine_bin = app
        .config
        .engine
        .bin
        .clone()
        .unwrap_or_else(|| "server".to_owned());
    build_engine(app, args.release_engine, &engine_bin)?;
    let artifact = package_mod(app, args.release_mod, args.target.as_deref())?;
    install_test_artifact(app, &artifact)?;
    if args.hot {
        watch_and_restart(app, args, &engine_bin)
    } else {
        run_engine_binary(app, args.release_engine, &engine_bin)
    }
}

fn build_engine(app: &App, release: bool, bin: &str) -> Result<()> {
    sync_engine(app)?;

    let worktree = app.engine_worktree_dir();
    let mut args = vec![
        "build".to_owned(),
        "-p".to_owned(),
        "engine".to_owned(),
        "--bin".to_owned(),
        bin.to_owned(),
    ];

    if release {
        args.push("--release".to_owned());
    }

    if !app.config.engine.features.is_empty() {
        args.push("--features".to_owned());
        args.push(app.config.engine.features.join(","));
    }

    run_command("cargo", &args, &worktree)
        .with_context(|| format!("failed to build Engine binary `{bin}`"))?;

    println!("Built Engine binary `{}` in {}", bin, worktree.display());
    Ok(())
}

fn build_mod(app: &App, release: bool, target: Option<&str>) -> Result<()> {
    let mut args = vec!["build".to_owned()];
    if release {
        args.push("--release".to_owned());
    }
    if let Some(target) = target {
        args.push("--target".to_owned());
        args.push(target.to_owned());
    }

    run_command("cargo", &args, &app.project_root).context("failed to build mod project")?;
    Ok(())
}

fn package_mod(app: &App, release: bool, target: Option<&str>) -> Result<PathBuf> {
    build_mod(app, release, target)?;
    package_existing_build(app, release, target, false)
}

fn package_existing_build(
    app: &App,
    release: bool,
    target: Option<&str>,
    include_target_suffix: bool,
) -> Result<PathBuf> {
    let artifact_source = locate_mod_artifact(app, release, target)?;
    let platform = infer_platform(target);
    let staged_name = match platform {
        PlatformKind::Windows => "mod.dll",
        PlatformKind::MacOs | PlatformKind::Unix => "mod.so",
    };

    let output_path = app.artifact_path_for(release, target, include_target_suffix);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let staging_parent = output_path
        .parent()
        .ok_or_else(|| anyhow!("invalid artifact output path {}", output_path.display()))?;
    let staging_dir = tempdir_in(staging_parent)
        .with_context(|| format!("failed to create {}", staging_parent.display()))?;
    let staged_path = staging_dir.path().join(staged_name);
    fs::copy(&artifact_source, &staged_path).with_context(|| {
        format!(
            "failed to stage built library from {} to {}",
            artifact_source.display(),
            staged_path.display()
        )
    })?;

    let archive_file = File::create(&output_path)
        .with_context(|| format!("failed to create archive {}", output_path.display()))?;
    let mut builder = Builder::new(archive_file);
    builder
        .append_path_with_name(&staged_path, staged_name)
        .with_context(|| format!("failed to add `{staged_name}` to archive"))?;
    builder.finish().context("failed to finalize .rf archive")?;

    println!("Packaged {}", output_path.display());
    Ok(output_path)
}

fn package_mod_targets(
    app: &App,
    release: bool,
    target: Option<&str>,
    all_platforms: bool,
) -> Result<Vec<PathBuf>> {
    ensure!(
        !(all_platforms && target.is_some()),
        "cannot combine `--all-platforms` with `--target`"
    );

    let targets = if all_platforms {
        vec![
            Some("x86_64-unknown-linux-gnu"),
            Some("x86_64-apple-darwin"),
            Some("x86_64-pc-windows-msvc"),
        ]
    } else {
        vec![target]
    };
    let include_target_suffix = all_platforms;
    let mut artifacts = Vec::with_capacity(targets.len());

    for target in targets {
        build_mod(app, release, target)?;
        artifacts.push(package_existing_build(
            app,
            release,
            target,
            include_target_suffix,
        )?);
    }

    Ok(artifacts)
}

fn install_test_artifact(app: &App, artifact: &Path) -> Result<PathBuf> {
    let mods_dir = app.engine_worktree_dir().join("mods");
    fs::create_dir_all(&mods_dir)
        .with_context(|| format!("failed to create {}", mods_dir.display()))?;

    let destination = mods_dir.join(format!("{}.rf", app.mod_id()));
    fs::copy(artifact, &destination).with_context(|| {
        format!(
            "failed to install {} into {}",
            artifact.display(),
            destination.display()
        )
    })?;

    println!("Installed {}", destination.display());
    Ok(destination)
}

fn run_engine_binary(app: &App, release: bool, bin: &str) -> Result<()> {
    let mut child = spawn_engine_binary(app, release, bin)?;
    let status = child
        .wait()
        .context("failed while waiting for Engine process")?;
    if !status.success() {
        bail!("Engine exited with status {status}");
    }

    Ok(())
}

fn spawn_engine_binary(app: &App, release: bool, bin: &str) -> Result<Child> {
    let worktree = app.engine_worktree_dir();
    let metadata = MetadataCommand::new()
        .current_dir(&worktree)
        .no_deps()
        .exec()
        .context("failed to load cargo metadata for Engine checkout")?;
    let target_dir = metadata.target_directory.into_std_path_buf();

    let binary_name = if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_owned()
    };
    let profile_dir = if release { "release" } else { "debug" };
    let binary_path = target_dir.join(profile_dir).join(binary_name);

    ensure!(
        binary_path.exists(),
        "Engine binary not found at {}; expected cargo build output",
        binary_path.display()
    );

    let mut command = Command::new(&binary_path);
    command.current_dir(&worktree);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());

    command
        .spawn()
        .with_context(|| format!("failed to launch {}", binary_path.display()))
}

fn watch_and_restart(app: &App, args: &DevArgs, engine_bin: &str) -> Result<()> {
    let mut current_app = app.clone();
    let mut current_engine_bin = engine_bin.to_owned();
    let mut child = spawn_engine_binary(&current_app, args.release_engine, &current_engine_bin)?;
    let mut state = scan_watch_state(&app.project_root)?;
    println!("Watching {} for changes", app.project_root.display());

    loop {
        if let Some(status) = child.try_wait().context("failed to poll Engine process")? {
            bail!("Engine exited with status {status}");
        }

        thread::sleep(Duration::from_secs(1));
        let next_state = scan_watch_state(&app.project_root)?;
        if next_state == state {
            continue;
        }
        state = next_state;

        println!("Change detected, rebuilding mod");
        let reloaded_app = App::load_from(&app.project_root)
            .context("failed to reload project state after change")?;
        let reloaded_engine_bin = reloaded_app
            .config
            .engine
            .bin
            .clone()
            .unwrap_or_else(|| "server".to_owned());
        let engine_changed = reloaded_app.engine.commit != current_app.engine.commit;
        let engine_bin_changed = reloaded_engine_bin != current_engine_bin;

        match (|| -> Result<()> {
            if engine_changed || engine_bin_changed {
                sync_engine(&reloaded_app)?;
                build_engine(&reloaded_app, args.release_engine, &reloaded_engine_bin)?;
            }

            let artifact = package_mod(&reloaded_app, args.release_mod, args.target.as_deref())?;
            install_test_artifact(&reloaded_app, &artifact)?;
            restart_engine_child(
                &mut child,
                &reloaded_app,
                args.release_engine,
                &reloaded_engine_bin,
            )?;
            Ok(())
        })() {
            Ok(_) => {
                current_app = reloaded_app;
                current_engine_bin = reloaded_engine_bin;
            }
            Err(error) => {
                eprintln!("hot reload failed: {error:#}");
            }
        }
    }
}

fn restart_engine_child(child: &mut Child, app: &App, release: bool, bin: &str) -> Result<()> {
    stop_engine_child(child)?;
    *child = spawn_engine_binary(app, release, bin)?;
    println!("Engine restarted");
    Ok(())
}

fn stop_engine_child(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }

    child.kill().context("failed to stop Engine process")?;
    let _ = child.wait().context("failed to reap Engine process")?;
    Ok(())
}

fn scan_watch_state(root: &Path) -> Result<BTreeMap<PathBuf, FileStamp>> {
    let mut state = BTreeMap::new();
    scan_watch_dir(root, root, &mut state)?;
    Ok(state)
}

fn scan_watch_dir(root: &Path, dir: &Path, state: &mut BTreeMap<PathBuf, FileStamp>) -> Result<()> {
    for entry in
        fs::read_dir(dir).with_context(|| format!("failed to read directory {}", dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to read directory entry in {}", dir.display()))?;
        let path = entry.path();
        if should_ignore_watch_path(root, &path) {
            continue;
        }

        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        if metadata.is_dir() {
            scan_watch_dir(root, &path, state)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("failed to relativize {}", path.display()))?
            .to_path_buf();
        state.insert(relative, stamp_for(&metadata));
    }
    Ok(())
}

fn should_ignore_watch_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    if relative.as_os_str().is_empty() {
        return false;
    }

    let first = relative
        .components()
        .next()
        .map(|component| component.as_os_str());
    if matches!(
        first.and_then(|value| value.to_str()),
        Some(".git" | ".rustforge" | "target" | "dist")
    ) {
        return true;
    }

    relative
        .components()
        .any(|component| component.as_os_str() == "target")
}

fn stamp_for(metadata: &fs::Metadata) -> FileStamp {
    let modified_millis = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    FileStamp {
        modified_millis,
        len: metadata.len(),
    }
}

fn sync_engine(app: &App) -> Result<()> {
    let mirror_dir = app.engine_mirror_dir();
    let mirror_parent = mirror_dir
        .parent()
        .ok_or_else(|| anyhow!("invalid mirror path {}", mirror_dir.display()))?;
    fs::create_dir_all(mirror_parent)
        .with_context(|| format!("failed to create {}", mirror_parent.display()))?;

    if !mirror_dir.exists() {
        run_command(
            "git",
            &[
                "clone".to_owned(),
                "--mirror".to_owned(),
                app.engine.repo_url.clone(),
                mirror_dir.display().to_string(),
            ],
            &app.project_root,
        )
        .with_context(|| format!("failed to clone Engine mirror from {}", app.engine.repo_url))?;
    } else {
        let remote_url = read_command_output("git", &["remote", "get-url", "origin"], &mirror_dir)
            .context("failed to inspect Engine mirror origin URL")?;
        if remote_url.trim() != app.engine.repo_url {
            run_command(
                "git",
                &[
                    "remote".to_owned(),
                    "set-url".to_owned(),
                    "origin".to_owned(),
                    app.engine.repo_url.clone(),
                ],
                &mirror_dir,
            )
            .with_context(|| {
                format!(
                    "failed to update Engine mirror origin to {}",
                    app.engine.repo_url
                )
            })?;
        }
    }

    run_command(
        "git",
        &[
            "fetch".to_owned(),
            "--prune".to_owned(),
            "origin".to_owned(),
        ],
        &mirror_dir,
    )
    .context("failed to fetch Engine mirror")?;

    if !mirror_has_commit(&mirror_dir, &app.engine.commit)? {
        run_command(
            "git",
            &[
                "fetch".to_owned(),
                "origin".to_owned(),
                app.engine.commit.clone(),
            ],
            &mirror_dir,
        )
        .with_context(|| format!("failed to fetch Engine commit {}", app.engine.commit))?;
    }

    ensure!(
        mirror_has_commit(&mirror_dir, &app.engine.commit)?,
        "Engine mirror does not contain required commit {} after fetch",
        app.engine.commit
    );

    ensure_worktree(app, &mirror_dir)
}

fn ensure_worktree(app: &App, mirror_dir: &Path) -> Result<()> {
    let worktree_dir = app.engine_worktree_dir();
    let parent = worktree_dir
        .parent()
        .ok_or_else(|| anyhow!("invalid worktree path {}", worktree_dir.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;

    let recreate = if worktree_dir.exists() {
        let head_matches = read_command_output("git", &["rev-parse", "HEAD"], &worktree_dir)
            .map(|head| head.trim() == app.engine.commit)
            .unwrap_or(false);
        let dirty = read_command_output("git", &["status", "--porcelain"], &worktree_dir)
            .map(|status| !status.trim().is_empty())
            .unwrap_or(true);
        !(head_matches && !dirty)
    } else {
        false
    };

    if recreate {
        remove_worktree(mirror_dir, &worktree_dir)?;
    }

    if !worktree_dir.exists() {
        run_command(
            "git",
            &[
                "worktree".to_owned(),
                "add".to_owned(),
                "--detach".to_owned(),
                worktree_dir.display().to_string(),
                app.engine.commit.clone(),
            ],
            mirror_dir,
        )
        .with_context(|| {
            format!(
                "failed to create Engine worktree at {}",
                worktree_dir.display()
            )
        })?;
    }

    let head = read_command_output("git", &["rev-parse", "HEAD"], &worktree_dir)
        .context("failed to verify Engine worktree commit")?;
    ensure!(
        head.trim() == app.engine.commit,
        "Engine worktree is at {}, expected {}",
        head.trim(),
        app.engine.commit
    );
    Ok(())
}

fn remove_worktree(mirror_dir: &Path, worktree_dir: &Path) -> Result<()> {
    let remove_result = Command::new("git")
        .current_dir(mirror_dir)
        .args(["worktree", "remove", "--force"])
        .arg(worktree_dir)
        .status();

    match remove_result {
        Ok(status) if status.success() => Ok(()),
        _ => {
            if worktree_dir.exists() {
                fs::remove_dir_all(worktree_dir)
                    .with_context(|| format!("failed to remove {}", worktree_dir.display()))?;
            }
            Ok(())
        }
    }
}

fn locate_mod_artifact(app: &App, release: bool, target: Option<&str>) -> Result<PathBuf> {
    let target_dir = app.metadata.target_directory.clone().into_std_path_buf();
    let profile = if release { "release" } else { "debug" };
    let platform = infer_platform(target);
    let crate_name = app.mod_cdylib_target_name()?;

    let filename = match platform {
        PlatformKind::Windows => format!("{crate_name}.dll"),
        PlatformKind::MacOs => format!("lib{crate_name}.dylib"),
        PlatformKind::Unix => format!("lib{crate_name}.so"),
    };

    let mut path = target_dir;
    if let Some(target) = target {
        path.push(target);
    }
    path.push(profile);
    path.push(filename);

    ensure!(
        path.exists(),
        "built mod artifact not found at {}; verify the package defines a cdylib target",
        path.display()
    );
    Ok(path)
}

fn load_config(manifest_path: &Path, project_root: &Path) -> Result<RustforgeConfig> {
    let manifest = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let value: toml::Value = toml::from_str(&manifest)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;

    if let Some(table) = value
        .get("package")
        .and_then(|value| value.get("metadata"))
        .and_then(|value| value.get("rustforge"))
    {
        return table
            .clone()
            .try_into()
            .context("failed to parse [package.metadata.rustforge]");
    }

    if let Some(table) = value
        .get("metadata")
        .and_then(|value| value.get("rustforge"))
    {
        return table
            .clone()
            .try_into()
            .context("failed to parse [metadata.rustforge]");
    }

    let fallback_path = project_root.join("rustforge.toml");
    if fallback_path.exists() {
        let fallback = fs::read_to_string(&fallback_path)
            .with_context(|| format!("failed to read {}", fallback_path.display()))?;
        return toml::from_str(&fallback)
            .with_context(|| format!("failed to parse {}", fallback_path.display()));
    }

    Ok(RustforgeConfig::default())
}

fn load_engine_spec(project_root: &Path, config: &RustforgeConfig) -> Result<EngineSpec> {
    let lock_path = project_root.join("Cargo.lock");
    ensure!(
        lock_path.exists(),
        "Cargo.lock not found at {}; run `cargo generate-lockfile` or `cargo build` first",
        lock_path.display()
    );

    let lock_contents = fs::read_to_string(&lock_path)
        .with_context(|| format!("failed to read {}", lock_path.display()))?;
    let lock_value: toml::Value =
        toml::from_str(&lock_contents).context("failed to parse Cargo.lock as TOML")?;
    let packages = lock_value
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| anyhow!("Cargo.lock does not contain a `package` array"))?;

    let mut matches = packages.iter().filter(|package| {
        package
            .get("name")
            .and_then(toml::Value::as_str)
            .map(|name| name == "enginelib")
            .unwrap_or(false)
    });

    let package = matches.next().ok_or_else(|| {
        anyhow!("`enginelib` not found in Cargo.lock; is this a mod project lockfile?")
    })?;
    ensure!(
        matches.next().is_none(),
        "multiple `enginelib` entries found in Cargo.lock; add configuration to disambiguate before continuing"
    );

    let source = package
        .get("source")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| anyhow!("`enginelib` entry in Cargo.lock is missing a `source` field"))?;

    let (derived_repo_url, commit) = parse_engine_source(source)?;
    ensure!(
        commit.len() >= 12,
        "engine commit `{commit}` is too short; expected full git SHA"
    );

    let repo_url = config
        .engine
        .repo_url
        .clone()
        .unwrap_or_else(|| derived_repo_url.clone());
    let commit_short = commit[..12].to_owned();

    Ok(EngineSpec {
        repo_url,
        commit,
        commit_short,
    })
}

fn parse_engine_source(source: &str) -> Result<(String, String)> {
    ensure!(
        source.starts_with("git+"),
        "Only git-pinned Engine/enginelib is supported; update dependency to a git source."
    );
    let stripped = &source[4..];
    let (repo_url, commit) = stripped.rsplit_once('#').ok_or_else(|| {
        anyhow!(
            "Only git-pinned Engine/enginelib is supported; missing commit in source `{source}`"
        )
    })?;
    ensure!(
        !repo_url.is_empty() && !commit.is_empty(),
        "Only git-pinned Engine/enginelib is supported; invalid source `{source}`"
    );
    Ok((repo_url.to_owned(), commit.to_owned()))
}

fn mirror_has_commit(mirror_dir: &Path, commit: &str) -> Result<bool> {
    let status = Command::new("git")
        .current_dir(mirror_dir)
        .args(["rev-parse", "--verify"])
        .arg(format!("{commit}^{{commit}}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| {
            format!(
                "failed to verify commit {commit} in {}",
                mirror_dir.display()
            )
        })?;
    Ok(status.success())
}

fn run_command(program: &str, args: &[String], current_dir: &Path) -> Result<()> {
    let status = Command::new(program)
        .current_dir(current_dir)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to start `{}`", format_command(program, args)))?;

    if !status.success() {
        bail!(
            "`{}` exited with status {}",
            format_command(program, args),
            status
        );
    }
    Ok(())
}

fn read_command_output(program: &str, args: &[&str], current_dir: &Path) -> Result<String> {
    let output = Command::new(program)
        .current_dir(current_dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("failed to start `{}`", format_command_refs(program, args)))?;
    if !output.status.success() {
        bail!(
            "`{}` exited with status {}",
            format_command_refs(program, args),
            output.status
        );
    }

    String::from_utf8(output.stdout).context("command output was not valid UTF-8")
}

fn resolve_configured_path(
    project_root: &Path,
    configured: Option<&str>,
    default: &str,
    commit: &str,
) -> PathBuf {
    let raw = configured.unwrap_or(default).replace("<sha>", commit);
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

fn infer_platform(target: Option<&str>) -> PlatformKind {
    match target {
        Some(target) if target.contains("windows") => PlatformKind::Windows,
        Some(target) if target.contains("apple-darwin") || target.contains("darwin") => {
            PlatformKind::MacOs
        }
        Some(_) => PlatformKind::Unix,
        None => match env::consts::OS {
            "windows" => PlatformKind::Windows,
            "macos" => PlatformKind::MacOs,
            _ => PlatformKind::Unix,
        },
    }
}

fn exists_or_missing(path: PathBuf) -> String {
    if path.exists() {
        path.display().to_string()
    } else {
        format!("missing ({})", path.display())
    }
}

fn format_command(program: &str, args: &[String]) -> String {
    format_args_iter(program, args.iter().map(String::as_str))
}

fn format_command_refs(program: &str, args: &[&str]) -> String {
    format_args_iter(program, args.iter().copied())
}

fn format_args_iter<'a>(program: &'a str, args: impl IntoIterator<Item = &'a str>) -> String {
    std::iter::once(program)
        .chain(args)
        .map(shell_escape)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_escape(segment: &str) -> String {
    if segment.is_empty() {
        return "''".to_owned();
    }
    if segment
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':' | '='))
    {
        segment.to_owned()
    } else {
        format!("'{}'", segment.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_source_with_commit() {
        let (repo, commit) = parse_engine_source(
            "git+https://github.com/voltaero/engine.git?branch=main#abcdef1234567890",
        )
        .expect("source should parse");
        assert_eq!(repo, "https://github.com/voltaero/engine.git?branch=main");
        assert_eq!(commit, "abcdef1234567890");
    }

    #[test]
    fn rejects_non_git_source() {
        let error = parse_engine_source("registry+https://example.com")
            .expect_err("non-git source should fail");
        assert!(
            error
                .to_string()
                .contains("Only git-pinned Engine/enginelib is supported")
        );
    }

    #[test]
    fn resolves_sha_placeholder_in_paths() {
        let root = Path::new("/tmp/project");
        let resolved = resolve_configured_path(
            root,
            Some(".rustforge/engine/worktrees/<sha>"),
            ".rustforge/engine/worktrees/default",
            "abc123",
        );
        assert_eq!(
            resolved,
            PathBuf::from("/tmp/project/.rustforge/engine/worktrees/abc123")
        );
    }

    #[test]
    fn parses_top_level_manifest_metadata() {
        let value: toml::Value = toml::from_str(
            r#"
            [metadata.rustforge]
            mod_id = "demo"

            [metadata.rustforge.engine]
            bin = "server"
            "#,
        )
        .expect("manifest should parse");

        let config: RustforgeConfig = value["metadata"]["rustforge"]
            .clone()
            .try_into()
            .expect("config should deserialize");
        assert_eq!(config.mod_id.as_deref(), Some("demo"));
        assert_eq!(config.engine.bin.as_deref(), Some("server"));
    }

    #[test]
    fn ignores_generated_paths_in_hot_watch() {
        let root = Path::new("/tmp/project");
        assert!(should_ignore_watch_path(
            root,
            Path::new("/tmp/project/.rustforge/out/mod.rf")
        ));
        assert!(should_ignore_watch_path(
            root,
            Path::new("/tmp/project/target/debug/libx.so")
        ));
        assert!(should_ignore_watch_path(
            root,
            Path::new("/tmp/project/rustforge/target/debug/rustforge")
        ));
        assert!(!should_ignore_watch_path(
            root,
            Path::new("/tmp/project/src/lib.rs")
        ));
    }
}
