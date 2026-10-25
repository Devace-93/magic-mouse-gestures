# Source-build formula used to seed the tap before the first binary release.
# `brew install Devace-93/tap/magic-mouse-gestures` builds with the Rust
# toolchain from Homebrew. The release workflow replaces this file with the
# binary formula generated from magic-mouse-gestures.rb.in.
class MagicMouseGestures < Formula
  desc "Apple Magic Mouse multitouch gestures for Linux/Wayland"
  homepage "https://github.com/Devace-93/magic-mouse-gestures"
  url "https://github.com/Devace-93/magic-mouse-gestures.git", branch: "main"
  version "0.1.0"
  license "MIT"
  head "https://github.com/Devace-93/magic-mouse-gestures.git", branch: "main"

  depends_on :linux
  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    bin.install "ui/magic-mouse-gestures-settings"
    (share/"applications").install "packaging/org.magicmouse.Gestures.desktop"
    pkgshare.install "packaging/60-magic-mouse-gestures.rules", "packaging/magic-mouse-gestures.conf"
    doc.install "README.md"
  end

  service do
    run [opt_bin/"magic-mouse-gestures", "run"]
    keep_alive true
    log_path var/"log/magic-mouse-gestures.log"
    error_log_path var/"log/magic-mouse-gestures.log"
  end

  def caveats
    <<~EOS
      The daemon needs access to the mouse and to /dev/uinput. Install the udev
      rule once (needs sudo):
        sudo install -Dm644 #{opt_pkgshare}/60-magic-mouse-gestures.rules /etc/udev/rules.d/60-magic-mouse-gestures.rules
        sudo install -Dm644 #{opt_pkgshare}/magic-mouse-gestures.conf /etc/modules-load.d/magic-mouse-gestures.conf
        sudo modprobe uinput && sudo udevadm control --reload-rules && sudo udevadm trigger
      Then start it in the background:
        brew services start magic-mouse-gestures
      The settings app needs python3-gi, GTK 4 and libadwaita from your distribution.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/magic-mouse-gestures --version")
  end
end
