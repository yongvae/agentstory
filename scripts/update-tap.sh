#!/usr/bin/env bash
# Render the Homebrew formula for a published release and push it to yongvae/homebrew-tap.
# Usage: scripts/update-tap.sh v0.2.2
set -euo pipefail
TAG=${1:?usage: update-tap.sh vX.Y.Z}
VER=${TAG#v}
REPO=yongvae/agentstory
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
gh release download "$TAG" -R "$REPO" -p SHA256SUMS -D "$TMP"
sha() { awk -v f="agentstory-$TAG-$1.tar.gz" '$2==f {print $1}' "$TMP/SHA256SUMS"; }
url() { echo "https://github.com/$REPO/releases/download/$TAG/agentstory-$TAG-$1.tar.gz"; }
for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  [ -n "$(sha $t)" ] || { echo "missing checksum for $t" >&2; exit 1; }
done
gh repo clone yongvae/homebrew-tap "$TMP/tap" -- -q
mkdir -p "$TMP/tap/Formula"
cat > "$TMP/tap/Formula/agentstory.rb" <<RUBY
class Agentstory < Formula
  desc "Lightweight agent-memory search: index and hand off sessions across coding agents"
  homepage "https://github.com/$REPO"
  version "$VER"
  license "MIT"

  on_macos do
    on_arm do
      url "$(url aarch64-apple-darwin)"
      sha256 "$(sha aarch64-apple-darwin)"
    end
    on_intel do
      url "$(url x86_64-apple-darwin)"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    on_arm do
      url "$(url aarch64-unknown-linux-gnu)"
      sha256 "$(sha aarch64-unknown-linux-gnu)"
    end
    on_intel do
      url "$(url x86_64-unknown-linux-gnu)"
      sha256 "$(sha x86_64-unknown-linux-gnu)"
    end
  end

  def install
    bin.install "agentstory", "ags"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/agentstory --version")
    assert_match "claude-code", shell_output("#{bin}/ags agents")
  end
end
RUBY
cd "$TMP/tap"
git add Formula/agentstory.rb
git -c user.name=yongvae -c user.email="$(gh api user --jq .id)+yongvae@users.noreply.github.com" commit -q -m "agentstory $VER"
git push -q
echo "pushed Formula/agentstory.rb for $TAG"
