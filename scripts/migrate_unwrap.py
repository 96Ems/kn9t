#!/usr/bin/env python3
"""
Migrate .lock().unwrap() and .lock().expect("...") to safe_expect!() macro.

Usage:
    python scripts/migrate_unwrap.py crates/kn9t-plugin/src/host.rs
    python scripts/migrate_unwrap.py --check crates/  # dry-run, list files needing migration
"""

import re
import sys
from pathlib import Path

# Patterns to find .lock()/.read()/.write().unwrap() or .expect("...")
# This handles the expression before .lock() by finding balanced parens/brackets
LOCK_UNWRAP = re.compile(r'\.(lock|read|write)\(\)\.unwrap\(\)')
LOCK_EXPECT = re.compile(r'\.(lock|read|write)\(\)\.expect\(("[^"]*")\)')

def find_expr_start(content: str, lock_pos: int) -> int:
    """Find where the expression before .lock() starts."""
    # Walk backwards from lock_pos to find the start of the expression
    # An expression starts after: = , ; { ( [ or newline with leading whitespace
    
    i = lock_pos - 1
    paren_depth = 0
    bracket_depth = 0
    
    while i >= 0:
        c = content[i]
        
        # Track nested parens/brackets going backwards
        if c == ')':
            paren_depth += 1
        elif c == '(':
            if paren_depth > 0:
                paren_depth -= 1
            else:
                # This ( is the start boundary
                return i + 1
        elif c == ']':
            bracket_depth += 1
        elif c == '[':
            if bracket_depth > 0:
                bracket_depth -= 1
            else:
                return i + 1
        elif c in '=,;{' and paren_depth == 0 and bracket_depth == 0:
            # Found a boundary
            return i + 1
        elif c == '\n' and paren_depth == 0 and bracket_depth == 0:
            # Check if next non-whitespace is the start
            j = i + 1
            while j < lock_pos and content[j] in ' \t':
                j += 1
            if j < lock_pos:
                return j
        
        i -= 1
    
    return 0

def migrate_content(content: str) -> tuple[str, int]:
    """Migrate all .lock().unwrap() and .lock().expect() to safe_expect!().
    
    Returns (new_content, count_of_replacements).
    """
    count = 0
    
    # Process .lock()/.read()/.write().unwrap() first
    while True:
        m = LOCK_UNWRAP.search(content)
        if not m:
            break
        
        method = m.group(1)  # lock, read, or write
        lock_pos = m.start()
        expr_start = find_expr_start(content, lock_pos)
        expr = content[expr_start:lock_pos].strip()
        
        # Handle leading * dereference — keep it outside the macro
        prefix = ''
        if expr.startswith('*'):
            prefix = '*'
            expr = expr[1:].strip()
        
        # Build replacement
        before = content[:expr_start]
        after = content[m.end():]
        replacement = f'{prefix}safe_expect!({expr}.{method}(), "poisoned")'
        
        content = before + replacement + after
        count += 1
    
    # Process .lock()/.read()/.write().expect("...")
    while True:
        m = LOCK_EXPECT.search(content)
        if not m:
            break
        
        method = m.group(1)  # lock, read, or write
        lock_pos = m.start()
        expr_start = find_expr_start(content, lock_pos)
        expr = content[expr_start:lock_pos].strip()
        msg = m.group(2)  # The message string
        
        # Handle leading * dereference — keep it outside the macro
        prefix = ''
        if expr.startswith('*'):
            prefix = '*'
            expr = expr[1:].strip()
        
        before = content[:expr_start]
        after = content[m.end():]
        replacement = f'{prefix}safe_expect!({expr}.{method}(), {msg})'
        
        content = before + replacement + after
        count += 1
    
    return content, count

def add_import(content: str) -> str:
    """Add use kn9t_macros::safe_expect; if not present."""
    if 'use kn9t_macros::' in content:
        return content
    
    # Find first 'use ' line and add before it
    lines = content.split('\n')
    for i, line in enumerate(lines):
        stripped = line.lstrip()
        if stripped.startswith('use ') and not stripped.startswith('use kn9t_macros'):
            indent = line[:len(line) - len(stripped)]
            lines.insert(i, f'{indent}use kn9t_macros::safe_expect;')
            return '\n'.join(lines)
    
    return content

def process_file(path: Path, dry_run: bool = False) -> int:
    """Process a single file. Returns count of replacements."""
    content = path.read_text()
    
    # Check if there's anything to migrate
    if not LOCK_UNWRAP.search(content) and not LOCK_EXPECT.search(content):
        return 0
    
    new_content, count = migrate_content(content)
    
    if count > 0:
        new_content = add_import(new_content)
        
        if dry_run:
            print(f"  {path}: {count} replacements needed")
        else:
            path.write_text(new_content)
            print(f"  {path}: {count} replacements done")
    
    return count

def main():
    args = sys.argv[1:]
    dry_run = '--check' in args
    args = [a for a in args if a != '--check']
    
    if not args:
        print("Usage: migrate_unwrap.py [--check] <file_or_dir>...")
        sys.exit(1)
    
    total = 0
    for arg in args:
        p = Path(arg)
        if p.is_file() and p.suffix == '.rs':
            total += process_file(p, dry_run)
        elif p.is_dir():
            for rs in p.rglob('*.rs'):
                # Skip test files
                if '/tests/' in str(rs) or rs.name.endswith('_test.rs'):
                    continue
                total += process_file(rs, dry_run)
    
    action = "would replace" if dry_run else "replaced"
    print(f"\nTotal: {action} {total} .lock().unwrap()/.expect() calls")

if __name__ == '__main__':
    main()
