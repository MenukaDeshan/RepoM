# Security Policy

RepoM inspects local Git repositories, talks to the GitHub API using user-provided tokens, and launches local tools (`git`, File Explorer, VS Code, Terminal). We take security issues seriously.

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Instead, report vulnerabilities privately through GitHub Security Advisories:

1. Open the repository's **Security** tab on GitHub.
2. Select **Advisories** -> **Report a vulnerability** (or visit [Report a vulnerability](https://github.com/MenukaDeshan/RepoM/security/advisories/new)).
3. Include what is affected, steps to reproduce, and the security impact.

We will acknowledge your report within 7 days and coordinate a fix and disclosure timeline with you.

## What counts as a security issue

- **Credential or token exposure:** A GitHub token, PAT, or embedded remote URL credential leaking into config files, logs, UI diagnostics, or exported JSON/Markdown reports.
- **Path traversal or boundary bypass:** Cloning or writing files outside the configured workspace root, or bypassing directory-name validation.
- **Command or argument injection:** Crafted repository names, branch names, file paths, or remote URLs causing unexpected flags or commands when invoking `git`, File Explorer, VS Code, Terminal, or the browser.
- **Unintended destructive Git operations:** Pull or sync actions modifying dirty, untracked, or diverged repositories without explicit consent.
- **Insecure transport:** Sending GitHub API tokens over non-HTTPS connections to non-loopback hosts.

## Supported versions

Security fixes are released on the latest version of RepoM.
