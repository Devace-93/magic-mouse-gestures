# Source-build formula used to seed the tap before the first binary release.
# `brew install Devace-93/tap/mousetouch` builds with the Rust
# toolchain from Homebrew. The release workflow replaces this file with the
# binary formula generated from mousetouch.rb.in.
class Mousetouch < Formula
  desc "Apple Magic Mouse multitouch gestures for Linux/Wayland"
  homepage "https://github.com/Devace-93/mousetouch"
  url "https://github.com/Devace-93/mousetouch.git", branch: "main"
  version "0.1.0"
  license "MIT"
  head "https://github.com/Devace-93/mousetouch.git", branch: "main"

  depends_on :linux
  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    bin.install "ui/mousetouch-settings"
    (share/"applications").install "packaging/io.github.Devace_93.MouseTouch.desktop"
    (share/"icons/hicolor/scalable/apps").install "packaging/io.github.Devace_93.MouseTouch.svg"
    pkgshare.install "packaging/60-mousetouch.rules", "packaging/mousetouch.conf"
    doc.install "README.md"
  end

  service do
    run [opt_bin/"mousetouch", "run"]
    keep_alive true
    log_path var/"log/mousetouch.log"
    error_log_path var/"log/mousetouch.log"
  end

  def caveats
    <<~EOS
      The daemon needs access to the mouse and to /dev/uinput. Install the udev
      rule once (needs sudo):
        sudo install -Dm644 #{opt_pkgshare}/60-mousetouch.rules /etc/udev/rules.d/60-mousetouch.rules
        sudo install -Dm644 #{opt_pkgshare}/mousetouch.conf /etc/modules-load.d/mousetouch.conf
        sudo modprobe uinput && sudo udevadm control --reload-rules && sudo udevadm trigger
      Then start it in the background:
        brew services start mousetouch
      The settings app needs python3-gi, python3-gi-cairo, GTK 4 and libadwaita from
      your distribution. Desktops do not look in Homebrew's share directory, so the
      daemon adds the app-menu entry itself the first time the service starts
      (~/.local/share/applications/io.github.Devace_93.MouseTouch.desktop; delete it
      after `brew uninstall`).
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/mousetouch --version")
  end
end
