#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

LEGACY_ALLOWLIST=(
  $'src/deploy.rs\tT024'
  $'src/store.rs\tT025'
  $'src/source/archive.rs\tT016'
  $'src/source/cache.rs\tT016'
  $'src/source/git.rs\tT016'
  $'src/source/http.rs\tT016'
  $'src/source/import.rs\tT016'
  $'src/source/mod.rs\tT016'
  $'src/source/worktree.rs\tT016'
  $'src/sync/transitive.rs\tT029'
)

LEGACY_INFRA=(
  src/main.rs
)

if [[ "${1:-}" == "--print-allowlist" ]]; then
  printf '%s\n' "${LEGACY_ALLOWLIST[@]}"
  exit 0
fi

if [[ "${1:-}" == "--print-infra" ]]; then
  printf '%s\n' "${LEGACY_INFRA[@]}"
  exit 0
fi

SCAN_ROOT="${1:-$REPO_ROOT}"
SRC="$SCAN_ROOT/src"

if [[ ! -d "$SRC" ]]; then
  echo "arch-check: no src/ under $SCAN_ROOT — nothing to lint" >&2
  exit 0
fi

read -r -d '' STRIP_PL <<'PERL' || true
my $s = do { local $/; open my $fh, "<", $ARGV[0] or die "open $ARGV[0]: $!"; <$fh> };
my $n = length $s;
my $out = "";
my $i = 0;
while ($i < $n) {
  my $c  = substr($s, $i, 1);
  my $c2 = substr($s, $i, 2);
  if ($c2 eq "//") {
    $i += 2;
    $i++ while $i < $n && substr($s, $i, 1) ne "\n";
    next;
  }
  if ($c2 eq "/*") {
    $i += 2;
    while ($i < $n && substr($s, $i, 2) ne "*/") {
      $out .= "\n" if substr($s, $i, 1) eq "\n";
      $i++;
    }
    $i += 2;
    next;
  }
  if ($c eq "r" && $i + 1 < $n && (substr($s, $i + 1, 1) eq "\"" || substr($s, $i + 1, 1) eq "#")) {
    my $j = $i + 1;
    my $hashes = 0;
    $hashes++, $j++ while $j < $n && substr($s, $j, 1) eq "#";
    if ($j < $n && substr($s, $j, 1) eq "\"") {
      $j++;
      my $close = "\"" . ("#" x $hashes);
      $out .= "\"";
      while ($j < $n && substr($s, $j, length $close) ne $close) {
        $out .= "\n" if substr($s, $j, 1) eq "\n";
        $j++;
      }
      $j += length $close;
      $out .= "\"";
      $i = $j;
      next;
    }
  }
  if ($c eq "\"") {
    $out .= "\"";
    $i++;
    while ($i < $n && substr($s, $i, 1) ne "\"") {
      if (substr($s, $i, 1) eq "\\") {
        $out .= "\n" if substr($s, $i + 1, 1) eq "\n";
        $i += 2;
        next;
      }
      $out .= "\n" if substr($s, $i, 1) eq "\n";
      $i++;
    }
    $out .= "\"" if $i < $n;
    $i++;
    next;
  }
  if ($c eq "'") {
    if (substr($s, $i, 4) =~ /^'\\.'/) { $out .= "''"; $i += 4; next; }
    if (substr($s, $i, 3) =~ /^'.'/)   { $out .= "''"; $i += 3; next; }
    $out .= $c; $i++; next;
  }
  $out .= $c;
  $i++;
}
print $out;
PERL

read -r -d '' CFG_AWK <<'AWK' || true
function cnt(s, ch,   n, i) { n = 0; for (i = 1; i <= length(s); i++) if (substr(s, i, 1) == ch) n++; return n }
BEGIN { state = "normal"; depth = 0 }
{
  if (state == "skip") { depth += cnt($0, "{") - cnt($0, "}"); if (depth <= 0) state = "normal"; next }
  if (state == "pending") {
    o = cnt($0, "{"); c = cnt($0, "}")
    if (o > 0) { depth = o - c; state = (depth <= 0) ? "normal" : "skip"; next }
    if ($0 ~ /;/) { state = "normal"; next }
    next
  }
  if ($0 ~ /#\[ *cfg\( *test *\) *\]/) {
    o = cnt($0, "{"); c = cnt($0, "}")
    if (o > 0) { depth = o - c; state = (depth <= 0) ? "normal" : "skip"; next }
    if ($0 ~ /;/) { next }
    state = "pending"; next
  }
  print
}
AWK

read -r -d '' USES_PL <<'PERL' || true
sub expand {
  my ($str) = @_;
  my $i = index($str, "{");
  return ($str) if $i < 0;
  my $prefix = substr($str, 0, $i);
  my ($depth, $end) = (0, -1);
  for (my $j = $i; $j < length $str; $j++) {
    my $ch = substr($str, $j, 1);
    $depth++ if $ch eq "{";
    if ($ch eq "}") { $depth--; if ($depth == 0) { $end = $j; last } }
  }
  return ($str) if $end < 0;
  my $inner = substr($str, $i + 1, $end - $i - 1);
  my @parts;
  my $buf = "";
  my $d = 0;
  for my $ch (split //, $inner) {
    if    ($ch eq "{") { $d++ }
    elsif ($ch eq "}") { $d-- }
    if ($ch eq "," && $d == 0) { push @parts, $buf; $buf = "" }
    else { $buf .= $ch }
  }
  push @parts, $buf;
  my @out;
  for my $p (@parts) {
    $p =~ s/^\s+|\s+$//g;
    next if $p eq "";
    push @out, $prefix . $_ for expand($p);
  }
  return @out;
}
local $/;
my $s = <STDIN>;
while ($s =~ /\buse\s+([^;]+);/gs) {
  my $u = $1;
  $u =~ s/\s+/ /g;
  $u =~ s/\s*::\s*/::/g;
  $u =~ s/^\s+|\s+$//g;
  for my $leaf (expand $u) {
    $leaf =~ s/\s+as\s+\S+//g;
    $leaf =~ s/::self\b//g;
    $leaf =~ s/^\s+|\s+$//g;
    print "$leaf\n" if $leaf ne "";
  }
}
PERL

strip_source() {
  perl -e "$STRIP_PL" "$1" | awk "$CFG_AWK"
}

stripped_or_die() {
  local file="$1" out
  if ! out="$(strip_source "$file")"; then
    echo "arch-check: failed to preprocess $file" >&2
    exit 2
  fi
  printf '%s' "$out"
}

leaf_in_set() {
  local rest="$1" allowed="$2" it
  rest="${rest#\{}"; rest="${rest%\}}"
  local IFS=','
  local -a items
  read -ra items <<< "$rest"
  for it in "${items[@]}"; do
    it="${it%% as *}"
    it="${it// /}"
    [[ -z "$it" ]] && continue
    case " $allowed " in
      *" $it "*) : ;;
      *) return 1 ;;
    esac
  done
  return 0
}

projection_use_ok() {
  local u="${1#pub }"
  u="${u#pub(crate) }"
  case "$u" in
    self|self::*|super|super::*) return 0 ;;
    crate::error|crate::error::*) return 0 ;;
    crate::diagnostic|crate::diagnostic::*) return 0 ;;
    crate::projection|crate::projection::*) return 0 ;;
    globset|globset::*) return 0 ;;
    unicode_normalization|unicode_normalization::*) return 0 ;;
    crate::source::*) leaf_in_set "${u#crate::source::}" "SourcePath SourceInventory SourceEntryMeta SourceEntryKind" ;;
    # safe_relpath: pure path guard shared with kernel identity types; expires with the kernel leaf entry (T029).
    crate::kernel::*) leaf_in_set "${u#crate::kernel::}" "TargetName ArtifactName SourceName Commit safe_relpath" ;;
    crate::*|crate) return 1 ;;
    std::fs|std::fs::*|std::process|std::process::*|std::net|std::net::*|std::io|std::io::*|std::os|std::os::*) return 1 ;;
    std|std::*) return 0 ;;
    *) return 1 ;;
  esac
}

reconcile_use_ok() {
  local u="${1#pub }"
  u="${u#pub(crate) }"
  case "$u" in
    self|self::*) return 0 ;;
    super::model|super::model::*) return 0 ;;
    super|super::*) return 1 ;;
    crate::projection|crate::projection::*) return 0 ;;
    crate::sync::model|crate::sync::model::*) return 0 ;;
    crate::sync::*|crate::sync) return 1 ;;
    crate::*|crate) return 1 ;;
    std::fs|std::fs::*|std::process|std::process::*|std::net|std::net::*|std::io|std::io::*|std::os|std::os::*) return 1 ;;
    std|std::*) return 0 ;;
    *) return 1 ;;
  esac
}

source_use_ok() {
  local u="${1#pub }"
  u="${u#pub(crate) }"
  case "$u" in
    crate::projection|crate::projection::*) return 1 ;;
    crate::sync|crate::sync::*) return 1 ;;
    crate::store|crate::store::*) return 1 ;;
    crate::deploy|crate::deploy::*) return 1 ;;
    crate::config::target|crate::config::target::*) return 1 ;;
    crate::config::TemplateOptIn|crate::config::TemplateOptIn::*) return 1 ;;
    crate::config::*::TemplateOptIn|crate::config::*::TemplateOptIn::*) return 1 ;;
    *) return 0 ;;
  esac
}

FQ_IO_RE='\bstd::(fs|process|net|io|os)::'

violations=0

check_uses() {
  local file="$1" validator="$2" label="$3" rel="$4" uses u
  uses="$(stripped_or_die "$file" | perl -e "$USES_PL")"
  while IFS= read -r u; do
    [[ -z "$u" ]] && continue
    if ! "$validator" "$u"; then
      echo "arch-check: $label forbidden dependency in ${rel}: use $u" >&2
      violations=$((violations + 1))
    fi
  done <<< "$uses"
}

check_fq_crate() {
  local file="$1" validator="$2" label="$3" rel="$4" depth="${5:-3}" stripped path
  stripped="$(stripped_or_die "$file")"
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    if ! "$validator" "$path"; then
      echo "arch-check: $label forbidden fully-qualified crate path in ${rel}: $path" >&2
      violations=$((violations + 1))
    fi
  done < <(FQ_DEPTH="$depth" perl -e '
    local $/;
    my $s = <STDIN>;
    my $depth = $ENV{FQ_DEPTH};
    $s =~ s/\buse\s+[^;]+;//gs;
    $s =~ s/\s*::\s*/::/g;
    while ($s =~ /\bcrate((?:::[A-Za-z_]\w*)+)/g) {
      my @seg = split /::/, "crate$1";
      @seg = @seg[0 .. $depth - 1] if $depth > 0 && @seg > $depth;
      print join("::", @seg), "\n";
    }
  ' <<< "$stripped")
}

check_fq_io() {
  local file="$1" label="$2" rel="$3" stripped re alias
  stripped="$(stripped_or_die "$file")"
  re="$FQ_IO_RE"
  while IFS= read -r alias; do
    [[ -z "$alias" ]] && continue
    re="$re|\\b${alias}::(fs|process|net|io|os)::"
  done < <(perl -ne 'print "$1\n" while /\buse\s+(?:std|crate)\s+as\s+([A-Za-z_]\w*)\s*;/g' <<< "$stripped")
  if grep -Eq "$re" <<< "$stripped"; then
    echo "arch-check: $label reaches std I/O via a fully-qualified path in ${rel}" >&2
    violations=$((violations + 1))
  fi
}

while IFS= read -r f; do
  if ! strip_source "$f" >/dev/null 2>&1; then
    echo "arch-check: failed to preprocess $f" >&2
    exit 2
  fi
done < <(find "$SRC" -type f -name '*.rs' | sort)

if [[ -d "$SRC/projection" ]]; then
  while IFS= read -r f; do
    case "$f" in */tests.rs | *_tests.rs) continue ;; esac
    rel="${f#"$SCAN_ROOT"/}"
    check_uses "$f" projection_use_ok "projection" "$rel"
    check_fq_io "$f" "projection" "$rel"
    check_fq_crate "$f" projection_use_ok "projection" "$rel"
  done < <(find "$SRC/projection" -type f -name '*.rs' | sort)
fi

recon="$SRC/sync/reconcile.rs"
if [[ -f "$recon" ]]; then
  rel="${recon#"$SCAN_ROOT"/}"
  check_uses "$recon" reconcile_use_ok "reconcile" "$rel"
  check_fq_io "$recon" "reconcile" "$rel"
  if grep -Eq '\.(exists|try_exists|metadata|symlink_metadata|is_file|is_dir|read_dir|read_link|canonicalize)[[:space:]]*\(' <<< "$(stripped_or_die "$recon")"; then
    echo "arch-check: reconcile reaches the filesystem via a Path fs-method in ${rel}" >&2
    violations=$((violations + 1))
  fi
fi

if [[ -d "$SRC/source" ]]; then
  while IFS= read -r f; do
    case "$f" in */tests.rs | *_tests.rs) continue ;; esac
    rel="${f#"$SCAN_ROOT"/}"
    check_uses "$f" source_use_ok "source" "$rel"
    check_fq_crate "$f" source_use_ok "source" "$rel" 0
  done < <(find "$SRC/source" -type f -name '*.rs' | sort)
fi

LEAK_RE='\bstd::fs\b|\bfs::[A-Za-z]|\bgix::|\bstd::process\b|\bstd::net\b|OpenOptions|File::(create|open)'

inv3_exempt() {
  local rel="$1" entry
  case "$rel" in
    */tests.rs | *_tests.rs) return 0 ;;
    src/projection/*|src/sync/*|src/cli/*) return 0 ;;
  esac
  for entry in "${LEGACY_ALLOWLIST[@]}"; do
    [[ "$rel" == "${entry%%$'\t'*}" ]] && return 0
  done
  for entry in "${LEGACY_INFRA[@]}"; do
    [[ "$rel" == "$entry" ]] && return 0
  done
  return 1
}

while IFS= read -r f; do
  rel="${f#"$SCAN_ROOT"/}"
  inv3_exempt "$rel" && continue
  if grep -Eq "$LEAK_RE" <<< "$(stripped_or_die "$f")"; then
    echo "arch-check: new module ${rel} performs target-side I/O outside the migration allowlist" >&2
    violations=$((violations + 1))
  fi
done < <(find "$SRC" -type f -name '*.rs' | sort)

if (( violations > 0 )); then
  echo "arch-check: FAILED with $violations violation(s)" >&2
  exit 1
fi
echo "arch-check: OK" >&2
exit 0
