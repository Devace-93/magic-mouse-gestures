# Spec for Fedora COPR (builds from the tagged source tarball with the
# distribution's Rust toolchain). The release workflow also attaches a
# ready-made .rpm to every GitHub release, built with cargo-generate-rpm.
# COPR note: enable "internet access during builds" for the project, or
# vendor the crates, because `cargo build` downloads dependencies.
Name:           magic-mouse-gestures
Version:        0.1.0
Release:        1%{?dist}
Summary:        Apple Magic Mouse multitouch gestures for Linux/Wayland
License:        MIT
URL:            https://github.com/Devace-93/magic-mouse-gestures
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  systemd-rpm-macros
Requires:       systemd
Recommends:     python3-gobject
Recommends:     gtk4
Recommends:     libadwaita

%description
Userspace driver for the Apple Magic Mouse on Linux/Wayland. Decodes the
mouse's multitouch reports and replays them as touchpad gestures the desktop
already understands: two-finger swipes switch workspaces with the native
animation, pinch zooms, taps run configurable actions, and one-finger
scrolling is shaped to start cleanly. Includes an optional GTK settings app.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --locked

%install
install -Dm755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dm755 ui/%{name}-settings %{buildroot}%{_bindir}/%{name}-settings
install -Dm644 packaging/org.magicmouse.Gestures.desktop %{buildroot}%{_datadir}/applications/org.magicmouse.Gestures.desktop
install -Dm644 packaging/60-%{name}.rules %{buildroot}%{_udevrulesdir}/60-%{name}.rules
install -Dm644 packaging/%{name}.conf %{buildroot}%{_modulesloaddir}/%{name}.conf
install -Dm644 packaging/%{name}.service %{buildroot}%{_userunitdir}/%{name}.service

%post
%udev_rules_update
modprobe uinput 2>/dev/null || :
systemctl --global enable %{name}.service >/dev/null 2>&1 || :

%preun
if [ $1 -eq 0 ]; then
    systemctl --global disable %{name}.service >/dev/null 2>&1 || :
fi

%postun
%udev_rules_update

%files
%license LICENSE
%doc README.md
%{_bindir}/%{name}
%{_bindir}/%{name}-settings
%{_datadir}/applications/org.magicmouse.Gestures.desktop
%{_udevrulesdir}/60-%{name}.rules
%{_modulesloaddir}/%{name}.conf
%{_userunitdir}/%{name}.service

%changelog
* Sat Oct 25 2026 Enrique Magallon <dev.ace.93.03@gmail.com> - 0.1.0-1
- Initial package
