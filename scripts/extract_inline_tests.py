#!/usr/bin/env python3
"""
Extract inline test modules from Rust source files to separate test files.

Usage:
    python scripts/extract_inline_tests.py <source_file> <output_test_file>

Example:
    python scripts/extract_inline_tests.py crates/kn9t-server/src/policy.rs crates/kn9t-server/tests/unit_policy.rs
"""

import sys
import re
from pathlib import Path


def find_test_module(content: str) -> tuple[int, int, str] | None:
    """
    Find the #[cfg(test)] mod tests { ... } block.
    Returns (start_line, end_line, test_content) or None if not found.
    """
    lines = content.split('\n')
    
    in_test = False
    test_start = -1
    brace_depth = 0
    test_lines = []
    
    i = 0
    while i < len(lines):
        line = lines[i]
        
        # Look for #[cfg(test)] followed by mod tests
        if '#[cfg(test)]' in line and not in_test:
            # Check if next non-empty, non-attribute line is mod tests
            j = i + 1
            while j < len(lines):
                next_line = lines[j].strip()
                if not next_line or next_line.startswith('#['):
                    j += 1
                    continue
                if next_line.startswith('mod tests'):
                    in_test = True
                    test_start = i
                    # Find the opening brace
                    while j < len(lines) and '{' not in lines[j]:
                        test_lines.append(lines[j])
                        j += 1
                    if j < len(lines):
                        test_lines.append(lines[j])
                        brace_depth = lines[j].count('{') - lines[j].count('}')
                    i = j
                    break
                else:
                    break
        elif in_test:
            test_lines.append(line)
            brace_depth += line.count('{') - line.count('}')
            if brace_depth == 0:
                return (test_start, i, '\n'.join(test_lines))
        
        i += 1
    
    return None


def remove_test_module(content: str, start_line: int, end_line: int) -> str:
    """Remove the test module from the source content."""
    lines = content.split('\n')
    # Remove the #[cfg(test)] line and everything through end_line
    new_lines = lines[:start_line] + lines[end_line + 1:]
    # Remove trailing empty lines
    while new_lines and not new_lines[-1].strip():
        new_lines.pop()
    return '\n'.join(new_lines) + '\n'


def transform_test_content(test_content: str, crate_name: str, module_name: str) -> str:
    """
    Transform the test module content for standalone test file.
    - Remove mod tests { wrapper
    - Add appropriate imports
    - Keep the test functions
    """
    lines = test_content.split('\n')
    
    # Find the actual test content (skip mod tests { line)
    content_start = 0
    for i, line in enumerate(lines):
        if 'mod tests' in line:
            # Skip until we find the opening brace and past it
            for j in range(i, len(lines)):
                if '{' in lines[j]:
                    content_start = j + 1
                    break
            break
    
    # Find the content end (last closing brace)
    content_end = len(lines) - 1
    for i in range(len(lines) - 1, -1, -1):
        if lines[i].strip() == '}':
            content_end = i
            break
    
    inner_content = '\n'.join(lines[content_start:content_end])
    
    # Remove leading indentation (typically 4 spaces)
    dedented_lines = []
    for line in inner_content.split('\n'):
        if line.startswith('    '):
            dedented_lines.append(line[4:])
        else:
            dedented_lines.append(line)
    
    inner_content = '\n'.join(dedented_lines)
    
    # Replace use super::*; with use crate_name::module_name::*;
    inner_content = re.sub(
        r'use super::\*;',
        f'use {crate_name}::{module_name}::*;',
        inner_content
    )
    
    # Build the output
    header = f'''//! Unit tests extracted from src/{module_name}.rs
//!
//! These tests were originally inline in the source file and have been
//! extracted to keep production code free of test code.

#![allow(clippy::unwrap_used)]

'''
    
    return header + inner_content + '\n'


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        sys.exit(1)
    
    source_path = Path(sys.argv[1])
    output_path = Path(sys.argv[2])
    
    if not source_path.exists():
        print(f"Error: Source file not found: {source_path}")
        sys.exit(1)
    
    content = source_path.read_text(encoding='utf-8')
    
    result = find_test_module(content)
    if result is None:
        print(f"No #[cfg(test)] mod tests found in {source_path}")
        sys.exit(1)
    
    start_line, end_line, test_content = result
    
    # Determine crate and module names
    # e.g., crates/kn9t-server/src/policy.rs -> kn9t_server, policy
    parts = source_path.parts
    crate_idx = parts.index('crates') + 1 if 'crates' in parts else 0
    crate_name = parts[crate_idx].replace('-', '_')
    module_name = source_path.stem
    
    # Transform test content
    transformed = transform_test_content(test_content, crate_name, module_name)
    
    # Write output
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(transformed, encoding='utf-8')
    print(f"Extracted {end_line - start_line + 1} lines to {output_path}")
    
    # Optionally show what to remove from source
    print(f"\nTo complete extraction, remove lines {start_line + 1}-{end_line + 1} from {source_path}")
    print("(including the #[cfg(test)] attribute)")


if __name__ == '__main__':
    main()
