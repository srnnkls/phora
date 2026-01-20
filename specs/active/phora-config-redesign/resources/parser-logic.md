# URL Parser Logic

## Supported Input Formats

| Input | GitHub | Ref | Path |
|-------|--------|-----|------|
| `srnnkls/dotfiles` | srnnkls/dotfiles | (default) | |
| `srnnkls/dotfiles/.claude/skills` | srnnkls/dotfiles | (default) | .claude/skills |
| `https://github.com/srnnkls/dotfiles` | srnnkls/dotfiles | (default) | |
| `https://github.com/srnnkls/dotfiles/tree/main` | srnnkls/dotfiles | main | |
| `https://github.com/srnnkls/dotfiles/tree/main/.claude/skills` | srnnkls/dotfiles | main | .claude/skills |
| `https://github.com/srnnkls/dotfiles/tree/v1.0` | srnnkls/dotfiles | v1.0 | |
| `gitlab.com/company/repo/artifacts` | | (default) | artifacts |

## GitHub URL Patterns

```
https://github.com/{owner}/{repo}
https://github.com/{owner}/{repo}/tree/{ref}
https://github.com/{owner}/{repo}/tree/{ref}/{path...}
https://github.com/{owner}/{repo}/blob/{ref}/{path...}
```

## Parsing Algorithm

```
func ParseURL(input string) (*ParsedSource, error):
    // 1. Detect format
    if hasProtocol(input):
        return parseFullURL(input)
    if hasHost(input):  // gitlab.com/...
        return parseHostedShorthand(input)
    return parseGitHubShorthand(input)

func parseGitHubShorthand(input string) (*ParsedSource, error):
    // input: "owner/repo" or "owner/repo/path/to/dir"
    segments = split(input, "/")
    if len(segments) < 2:
        return error("invalid format")

    owner = segments[0]
    repo = segments[1]
    path = join(segments[2:], "/")

    return &ParsedSource{
        GitHub: owner + "/" + repo,
        Path:   path,
    }

func parseFullURL(input string) (*ParsedSource, error):
    // input: "https://github.com/owner/repo/tree/ref/path"
    url = parseURL(input)

    host = url.Host  // github.com, gitlab.com
    segments = split(url.Path, "/")  // ["owner", "repo", "tree", "ref", "path..."]

    owner = segments[0]
    repo = segments[1]

    var ref, path string
    if len(segments) > 2 && (segments[2] == "tree" || segments[2] == "blob"):
        ref = segments[3]
        path = join(segments[4:], "/")

    result = &ParsedSource{Path: path}

    switch host:
    case "github.com":
        result.GitHub = owner + "/" + repo
    case "gitlab.com":
        result.GitLab = owner + "/" + repo
    default:
        result.Git = input  // Use full URL

    // Determine ref type (heuristic)
    if ref != "":
        if looksLikeSHA(ref):
            result.Rev = ref
        else if looksLikeTag(ref):  // v1.0, 1.0.0
            result.Tag = ref
        else:
            result.Branch = ref

    return result
```

## Edge Cases

1. **Ref contains `/`**: Branch names like `feature/foo` - need careful splitting
2. **Path starts with version-like string**: `v1/api` vs tag `v1`
3. **No ref specified**: Default branch (don't set branch/tag/rev)
4. **Invalid URL**: Return error, don't panic
