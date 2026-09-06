"""Configuration loading for kn9t-skills plugin."""

from __future__ import annotations

import os
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class SkillsConfig:
    """Configuration for skills discovery and behavior."""
    
    # Directories to search for skills
    paths: list[Path] = field(default_factory=list)
    
    # Whether to automatically activate skills based on task matching
    auto_activate: bool = True


def get_config_dir() -> Path:
    """Get the kn9t config directory (~/.kn9t)."""
    if home := os.environ.get("HOME"):
        return Path(home) / ".kn9t"
    if home := os.environ.get("USERPROFILE"):
        return Path(home) / ".kn9t"
    return Path.cwd() / ".kn9t"


def get_project_config_dir() -> Path:
    """Get project-local config directory (.kn9t in cwd)."""
    return Path.cwd() / ".kn9t"


def expand_path(path_str: str) -> Path:
    """Expand ~ and environment variables in a path."""
    return Path(os.path.expanduser(os.path.expandvars(path_str)))


def get_default_skill_paths() -> list[Path]:
    """Get all default skill search paths.
    
    Supports multiple conventions:
    - ~/.kn9t/skills/ (global)
    - .kn9t/skills/ (project-local, kn9t convention)
    - .agents/skills/ (project-local, common convention)
    - .agents/skill/ (singular variant)
    - .skills/ (simple hidden)
    - .skill/ (singular)
    - skills/ (visible)
    - skill/ (singular)
    """
    cwd = Path.cwd()
    return [
        # Global
        get_config_dir() / "skills",
        # Project-local variants
        cwd / ".kn9t" / "skills",
        cwd / ".agents" / "skills",
        cwd / ".agents" / "skill",
        cwd / ".skills",
        cwd / ".skill",
        cwd / "skills",
        cwd / "skill",
    ]


def load_config() -> SkillsConfig:
    """Load skills configuration from ~/.kn9t/skills.toml and defaults.
    
    Searches all common skill directory conventions by default.
    """
    config = SkillsConfig()
    
    # Default paths - all conventions
    config.paths = get_default_skill_paths()
    
    # Load config file if it exists
    config_file = get_config_dir() / "skills.toml"
    if config_file.exists():
        try:
            with open(config_file, "rb") as f:
                data = tomllib.load(f)
            
            # Override paths if specified (adds to defaults, doesn't replace)
            if "paths" in data:
                extra_paths = [expand_path(p) for p in data["paths"]]
                # Prepend custom paths so they take priority
                config.paths = extra_paths + [p for p in config.paths if p not in extra_paths]
            
            if "auto_activate" in data:
                config.auto_activate = bool(data["auto_activate"])
            
            print(f"Loaded config from {config_file}", file=sys.stderr)
            
        except Exception as e:
            print(f"Warning: Failed to load {config_file}: {e}", file=sys.stderr)
    
    return config
