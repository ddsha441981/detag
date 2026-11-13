#!/usr/bin/env python3
"""Lint commit messages for Detag's conventional-commit policy (P0-006, NFR-52).

Rules enforced per commit (merge commits are skipped):
  1. Subject matches: <type>(<scope>): <id> <description>
     - type: feat | fix | chore | docs | test | refactor | perf | ci | build | revert
     - scope: one of the vocabulary in CONTRIBUTING.md ("Commits" section)
       (auth, engine, api, worker, web, ops, core, storage, billing,
       mailer, docs, repo)
     - id: a ticket id P<phase>-<num> (e.g. P0-006, P1-014) — or, for
       work that has no P-ticket, an issue id CI-FAIL-<n> (a CI failure
       fix, COMMITS-LINT-001) or SCHEMA-DOC-<n> (a schema-docs drift
       fix) in the same position.
  2. The full message (subject + body) references at least one requirement
     id: FR-<area>-<n> (e.g. FR-D-05) or NFR-<n> (e.g. NFR-52).
  3. Subject is at most 100 characters.

Usage:
  lint_commits.py [--range REV_RANGE] [--single REV]
      Lint every commit in REV_RANGE (a git rev-list argument, e.g.
      "main~5..HEAD", "abc123..def456"). Default range: HEAD.
      --single REV lints exactly one commit (e.g. --single HEAD).
      Exit 0 when all commits pass, 1 otherwise.

Note: `--range HEAD` means "all history reachable from HEAD". The
pre-policy root commit ("docs: import PureFrame agent kit") does not
satisfy the rule; CI always passes a bounded range (push: before..head,
PR: base..head), so it is never linted there.

The GitHub workflow .github/workflows/commits.yml calls this on every push
and pull request, so the rule is enforced in CI as well as locally
(`just lint-commits`).
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys

TYPES = {
    "feat",
    "fix",
    "chore",
    "docs",
    "test",
    "refactor",
    "perf",
    "ci",
    "build",
    "revert",
}

# Scope vocabulary (documented in CONTRIBUTING.md, "Commits" section).
SCOPES = {
    "auth",
    "engine",
    "api",
    "worker",
    "web",
    "ops",
    "core",
    "storage",
    "billing",
    "mailer",
    "docs",
    "repo",
}

# The id in the ticket position: a P-ticket, or an issue id for work
# that has no P-ticket (COMMITS-LINT-001 — a CI-fix or schema-docs
# commit cannot name a P-ticket it does not belong to).
ID_RE = r"(?:P\d+-\d+|CI-FAIL-\d+|SCHEMA-DOC-\d+)"

SUBJECT_RE = re.compile(
    r"^(?P<type>[a-z]+)"
    r"\((?P<scope>[a-z][a-z0-9-]*)\)"
    rf": (?P<ticket>{ID_RE}) (?P<desc>\S.*)$"
)
TICKET_RE = re.compile(ID_RE)
REQUIREMENT_RE = re.compile(r"\b(?:FR(?:-[A-Z]+)?-\d+|NFR-\d+)\b")
MAX_SUBJECT_LEN = 100


def check_message(message: str) -> list[str]:
    """Return a list of policy violations for one commit message (empty = pass)."""
    errors: list[str] = []
    lines = message.strip("\n").split("\n")
    subject = lines[0] if lines else ""

    if len(subject) > MAX_SUBJECT_LEN:
        errors.append(
            f"subject is {len(subject)} chars (max {MAX_SUBJECT_LEN})"
        )

    match = SUBJECT_RE.match(subject)
    if not match:
        errors.append(
            "subject must match '<type>(<scope>): <id> <description>' "
            "(id: P<phase>-<num>, CI-FAIL-<n> or SCHEMA-DOC-<n>) "
            "(e.g. 'feat(auth): P1-014 refresh token rotation' or "
            "'fix(ops): CI-FAIL-004 replace dead MinIO with Garage S3')"
        )
        return errors  # Without a parseable subject the rest is noise.

    ctype = match.group("type")
    scope = match.group("scope")
    if ctype not in TYPES:
        errors.append(
            f"unknown type '{ctype}' (allowed: {', '.join(sorted(TYPES))})"
        )
    if scope not in SCOPES:
        errors.append(
            f"unknown scope '{scope}' (allowed: {', '.join(sorted(SCOPES))})"
        )

    if not TICKET_RE.search(subject):
        errors.append(
            "subject must contain a ticket id "
            "(P<phase>-<num>, CI-FAIL-<n> or SCHEMA-DOC-<n>)"
        )

    if not REQUIREMENT_RE.search(message):
        errors.append(
            "message must reference at least one requirement id "
            "(e.g. FR-D-05 or NFR-52)"
        )
    return errors


def commit_messages(
    rev_range: str = "HEAD", single: str | None = None
) -> list[tuple[str, str]]:
    """Return [(sha, full message)] for commits in rev_range (oldest first).

    With single=REV, return exactly that one commit.
    """
    cmd = ["git", "log", "--reverse", "--format=%H%x1f%B%x1e"]
    if single is not None:
        cmd += ["-1", single]
    else:
        cmd.append(rev_range)
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"git log failed for range {rev_range!r}: {result.stderr.strip()}")
    commits: list[tuple[str, str]] = []
    for record in result.stdout.split("\x1e"):
        record = record.strip("\n")
        if not record:
            continue
        sha, sep, message = record.partition("\x1f")
        if not sep:
            continue
        commits.append((sha.strip(), message.strip("\n")))
    return commits


def is_merge(message: str) -> bool:
    return message.split("\n", 1)[0].startswith("Merge ")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--range",
        default="HEAD",
        help="git rev-list range to lint (default: HEAD)",
    )
    parser.add_argument(
        "--single",
        default=None,
        metavar="REV",
        help="lint exactly one commit instead of a range",
    )
    args = parser.parse_args(argv)

    try:
        commits = commit_messages(args.range, single=args.single)
    except RuntimeError as exc:
        print(f"lint-commits: {exc}", file=sys.stderr)
        return 2

    if not commits:
        print(f"lint-commits: no commits in range {args.range!r}; nothing to check")
        return 0

    failed = 0
    for sha, message in commits:
        subject = message.split("\n", 1)[0]
        if is_merge(message):
            print(f"ok (merge, skipped): {sha[:7]} {subject}")
            continue
        errors = check_message(message)
        if errors:
            failed += 1
            print(f"FAIL: {sha[:7]} {subject}")
            for error in errors:
                print(f"  - {error}")
        else:
            print(f"ok: {sha[:7]} {subject}")

    if failed:
        print(f"lint-commits: {failed} of {len(commits)} commit(s) failed the policy")
        return 1
    print(f"lint-commits: all {len(commits)} commit(s) pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
