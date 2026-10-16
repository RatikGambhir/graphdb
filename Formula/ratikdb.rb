# Homebrew formula for RatikDB.
#
# Use it from a tap:
#   brew tap ratikgambhir/ratikdb https://github.com/RatikGambhir/graphdb
#   brew install --HEAD ratikgambhir/ratikdb/ratikdb   # until a release is tagged
#   brew services start ratikdb     # runs `ratik serve` at login via launchd
#
# Or straight from this checkout:
#   brew install --HEAD --build-from-source ./Formula/ratikdb.rb
class Ratikdb < Formula
  desc "Strictly-typed graph database with vector and full-text search in one binary"
  homepage "https://github.com/RatikGambhir/graphdb"
  url "https://github.com/RatikGambhir/graphdb/archive/refs/tags/v0.1.0.tar.gz"
  # Fill in after tagging: shasum -a 256 v0.1.0.tar.gz
  # sha256 "..."
  license "Apache-2.0"
  head "https://github.com/RatikGambhir/graphdb.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/ratik-cli")
  end

  service do
    run [opt_bin/"ratik", "serve", "--data", var/"ratikdb", "--quiet"]
    keep_alive successful_exit: false
    log_path var/"log/ratikdb.log"
    error_log_path var/"log/ratikdb.log"
    working_dir var
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ratik --version")
    output = shell_output("#{bin}/ratik query --memory 'SELECT 1 + 1 AS two'")
    assert_match "2", output
  end
end
