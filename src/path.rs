use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct ProjectsDir {
    pub path: PathBuf,
    pub profile: String,
}

pub fn resolve_projects_dirs(
    cli_override: Option<&str>,
    config_file_dirs: Option<&[String]>,
) -> Result<Vec<ProjectsDir>> {
    if let Some(s) = cli_override {
        let dirs = parse_csv_paths(s, "--config-dir")?;
        return validate_and_build(&dirs, Source::CliFlag(s));
    }

    if let Some(paths) = config_file_dirs {
        if paths.is_empty() {
            anyhow::bail!(
                "config_dirs in config file is empty. Remove the key to use default discovery, \
                 or list at least one directory."
            );
        }
        let dirs: Vec<PathBuf> = paths.iter().map(|s| expand_tilde(s)).collect();
        return validate_and_build(&dirs, Source::ConfigFile);
    }

    if let Ok(env_paths) = env::var("CLAUDE_CONFIG_DIR") {
        let dirs = parse_csv_paths(&env_paths, "CLAUDE_CONFIG_DIR")?;
        return validate_and_build(&dirs, Source::Env(env_paths));
    }

    discover_defaults()
}

enum Source<'a> {
    CliFlag(&'a str),
    ConfigFile,
    Env(String),
}

fn parse_csv_paths(raw: &str, label: &str) -> Result<Vec<PathBuf>> {
    let dirs: Vec<PathBuf> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(expand_tilde)
        .collect();
    if dirs.is_empty() {
        anyhow::bail!("{label} is set but contains no valid paths");
    }
    Ok(dirs)
}

fn validate_and_build(dirs: &[PathBuf], source: Source<'_>) -> Result<Vec<ProjectsDir>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in dirs {
        let normalized = normalize_config_path(raw);
        let projects = normalized.join("projects");
        if projects.is_dir() && seen.insert(projects.clone()) {
            out.push(ProjectsDir {
                path: projects,
                profile: derive_profile_name(&normalized),
            });
        }
    }
    if out.is_empty() {
        let detail = match source {
            Source::CliFlag(s) => format!("--config-dir {s}"),
            Source::ConfigFile => "config file `config_dirs`".to_string(),
            Source::Env(s) => format!("CLAUDE_CONFIG_DIR={s}"),
        };
        anyhow::bail!(
            "No valid Claude data directories found via {detail}. \
             Each path must be a Claude config directory containing 'projects/', \
             or the 'projects/' directory itself."
        );
    }
    Ok(out)
}

fn discover_defaults() -> Result<Vec<ProjectsDir>> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let xdg = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for candidate in [xdg.join("claude"), home.join(".claude")] {
        let projects = candidate.join("projects");
        if projects.is_dir() && seen.insert(projects.clone()) {
            out.push(ProjectsDir {
                path: projects,
                profile: derive_profile_name(&candidate),
            });
        }
    }

    if out.is_empty() {
        anyhow::bail!(
            "No Claude projects directory found. Set CLAUDE_CONFIG_DIR, \
             write ~/.config/claude-history/config.toml, or ensure ~/.claude/projects exists."
        );
    }
    Ok(out)
}

fn normalize_config_path(path: &Path) -> PathBuf {
    if path.file_name().and_then(|n| n.to_str()) == Some("projects") && path.is_dir() {
        path.parent().map(PathBuf::from).unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn derive_profile_name(config_dir: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if config_dir == home.join(".claude") {
            return "default".to_string();
        }
        let xdg = env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".config"));
        if config_dir == xdg.join("claude") {
            return "default".to_string();
        }
    }
    config_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string())
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(s)
}

pub fn filter_by_profile(dirs: Vec<ProjectsDir>, profile: Option<&str>) -> Result<Vec<ProjectsDir>> {
    match profile {
        None => Ok(dirs),
        Some(p) => {
            let available: Vec<String> = dirs.iter().map(|d| d.profile.clone()).collect();
            let filtered: Vec<ProjectsDir> =
                dirs.into_iter().filter(|d| d.profile == p).collect();
            if filtered.is_empty() {
                anyhow::bail!(
                    "No projects directory matches profile '{p}'. Available: {}",
                    available.join(", ")
                );
            }
            Ok(filtered)
        }
    }
}

pub fn extract_project_info(file_path: &Path, projects_dirs: &[ProjectsDir]) -> (String, String) {
    for pd in projects_dirs {
        if let Ok(rel) = file_path.strip_prefix(&pd.path) {
            let project = rel
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .unwrap_or_default();
            return (project, pd.profile.clone());
        }
    }
    (String::new(), String::new())
}

pub fn find_jsonl_files(
    projects_dirs: &[ProjectsDir],
    project_filter: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for pd in projects_dirs {
        for entry in WalkDir::new(&pd.path).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.extension().is_some_and(|ext| ext == "jsonl") {
                continue;
            }
            if let Some(filter) = project_filter {
                if let Ok(rel) = path.strip_prefix(&pd.path) {
                    let project_dir = rel
                        .components()
                        .next()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .unwrap_or_default();
                    let project_path = project_dir.replace('-', "/");
                    if !project_path.contains(filter) && !project_dir.contains(filter) {
                        continue;
                    }
                }
            }
            files.push(path.to_path_buf());
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn normalize_strips_projects_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        fs::create_dir(&projects).unwrap();
        assert_eq!(normalize_config_path(&projects), dir.path());
    }

    #[test]
    fn normalize_keeps_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(normalize_config_path(dir.path()), dir.path().to_path_buf());
    }

    #[test]
    fn profile_name_for_dot_claude_is_default() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(derive_profile_name(&home.join(".claude")), "default");
    }

    #[test]
    fn profile_name_for_profile_dir_uses_basename() {
        let dir = PathBuf::from("/some/where/profiles/personal");
        assert_eq!(derive_profile_name(&dir), "personal");
    }

    #[test]
    fn expand_tilde_resolves_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~/foo/bar"), home.join("foo/bar"));
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn extract_project_info_strips_base() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let pd = ProjectsDir {
            path: projects.clone(),
            profile: "personal".to_string(),
        };
        let file = projects.join("-Users-me-foo").join("sess.jsonl");
        let (project, profile) = extract_project_info(&file, &[pd]);
        assert_eq!(project, "-Users-me-foo");
        assert_eq!(profile, "personal");
    }

    #[test]
    fn extract_project_info_returns_empty_when_no_match() {
        let pd = ProjectsDir {
            path: PathBuf::from("/nowhere"),
            profile: "x".to_string(),
        };
        let (project, profile) = extract_project_info(Path::new("/elsewhere/foo.jsonl"), &[pd]);
        assert!(project.is_empty());
        assert!(profile.is_empty());
    }

    #[test]
    fn filter_by_profile_matches() {
        let dirs = vec![
            ProjectsDir {
                path: PathBuf::from("/a"),
                profile: "personal".to_string(),
            },
            ProjectsDir {
                path: PathBuf::from("/b"),
                profile: "work".to_string(),
            },
        ];
        let result = filter_by_profile(dirs, Some("personal")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].profile, "personal");
    }

    #[test]
    fn filter_by_profile_errors_on_no_match() {
        let dirs = vec![ProjectsDir {
            path: PathBuf::from("/a"),
            profile: "personal".to_string(),
        }];
        assert!(filter_by_profile(dirs, Some("nope")).is_err());
    }

    #[test]
    fn find_jsonl_files_walks_all_bases() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let pa = dir_a.path().join("projects/proj-a");
        let pb = dir_b.path().join("projects/proj-b");
        fs::create_dir_all(&pa).unwrap();
        fs::create_dir_all(&pb).unwrap();
        fs::write(pa.join("s1.jsonl"), "").unwrap();
        fs::write(pb.join("s2.jsonl"), "").unwrap();

        let dirs = vec![
            ProjectsDir {
                path: dir_a.path().join("projects"),
                profile: "a".to_string(),
            },
            ProjectsDir {
                path: dir_b.path().join("projects"),
                profile: "b".to_string(),
            },
        ];
        let files = find_jsonl_files(&dirs, None).unwrap();
        assert_eq!(files.len(), 2);
    }
}
