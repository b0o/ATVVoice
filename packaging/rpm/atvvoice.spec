%global debug_package %{nil}

Name:           atvvoice
Version:        __VERSION__
Release:        1%{?dist}
Summary:        Android TV Voice over BLE (ATVV) daemon for Linux/PipeWire

License:        MIT
URL:            https://github.com/b0o/ATVVoice
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-vendor-%{version}.tar.gz

BuildRequires:  rust
BuildRequires:  cargo
BuildRequires:  gcc
BuildRequires:  pkgconf-pkg-config
BuildRequires:  pipewire-devel
BuildRequires:  dbus-devel
BuildRequires:  bluez-libs-devel
BuildRequires:  clang-devel

Requires:       pipewire-libs
Requires:       dbus-libs
Requires:       bluez

ExclusiveArch:  x86_64 aarch64

%description
Userspace daemon that captures voice audio from BLE TV remotes using the
Google Voice over BLE (ATVV) protocol and exposes it as a PipeWire virtual
microphone source on Linux. Supports G20S Pro and other ATVV-compatible
remotes.

%prep
%autosetup -n %{name}-%{version}
tar xf %{SOURCE1}

%build
cargo build --release --offline --frozen

%install
install -Dpm 0755 target/release/atvvoice %{buildroot}%{_bindir}/atvvoice
install -Dpm 0644 dist/atvvoice.service %{buildroot}%{_userunitdir}/atvvoice.service

%files
%license LICENSE
%doc README.md
%{_bindir}/atvvoice
%{_userunitdir}/atvvoice.service

%changelog
* __CHANGELOG_DATE__ __MAINTAINER_NAME__ <__MAINTAINER_EMAIL__> - __VERSION__-1
- New upstream release __VERSION__
