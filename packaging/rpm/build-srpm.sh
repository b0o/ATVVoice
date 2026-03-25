#!/bin/bash
# Build a source RPM for atvvoice with vendored Cargo dependencies.
# Usage: ./build-srpm.sh --version 0.1.3 [--maintainer-name "Name"] [--maintainer-email "email"]
# Output: prints the path to the built SRPM on the last line.

set -euo pipefail

PACKAGE_NAME="atvvoice"
MAINTAINER_NAME="${MAINTAINER_NAME:-atvvoice Release Bot}"
MAINTAINER_EMAIL="${MAINTAINER_EMAIL:-noreply@github.com}"

usage() {
	echo "Usage: $0 --version VERSION [--maintainer-name NAME] [--maintainer-email EMAIL]"
	exit 1
}

while [[ $# -gt 0 ]]; do
	case $1 in
	--version)
		VERSION="$2"
		shift 2
		;;
	--maintainer-name)
		MAINTAINER_NAME="$2"
		shift 2
		;;
	--maintainer-email)
		MAINTAINER_EMAIL="$2"
		shift 2
		;;
	-h | --help) usage ;;
	*)
		echo "Unknown option: $1"
		usage
		;;
	esac
done

if [[ -z "${VERSION:-}" ]]; then
	echo "Error: --version is required"
	usage
fi

REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
SPEC_TEMPLATE="$(dirname "$0")/${PACKAGE_NAME}.spec"

if [[ ! -f "$SPEC_TEMPLATE" ]]; then
	echo "Error: spec template not found at $SPEC_TEMPLATE"
	exit 1
fi

echo "=== Building SRPM for ${PACKAGE_NAME}-${VERSION} ==="

# Set up RPM build tree
BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT
mkdir -p "$BUILD_DIR"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# Create source tarball from git
echo "--- Creating source tarball ---"
git -C "$REPO_ROOT" archive --format=tar.gz \
	--prefix="${PACKAGE_NAME}-${VERSION}/" \
	HEAD \
	>"$BUILD_DIR/SOURCES/${PACKAGE_NAME}-${VERSION}.tar.gz"

# Vendor Cargo dependencies
echo "--- Vendoring Cargo dependencies ---"
VENDOR_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR" "$VENDOR_DIR"' EXIT

(
	cd "$VENDOR_DIR"
	tar xf "$BUILD_DIR/SOURCES/${PACKAGE_NAME}-${VERSION}.tar.gz"
	cd "${PACKAGE_NAME}-${VERSION}"
	mkdir -p .cargo
	cargo vendor vendor/ >.cargo/config.toml
	tar czf "$BUILD_DIR/SOURCES/${PACKAGE_NAME}-vendor-${VERSION}.tar.gz" vendor/ .cargo/
)

# Generate spec from template
echo "--- Generating spec file ---"
CHANGELOG_DATE="$(date +'%a %b %d %Y')"
sed \
	-e "s/__VERSION__/${VERSION}/g" \
	-e "s/__CHANGELOG_DATE__/${CHANGELOG_DATE}/g" \
	-e "s/__MAINTAINER_NAME__/${MAINTAINER_NAME}/g" \
	-e "s/__MAINTAINER_EMAIL__/${MAINTAINER_EMAIL}/g" \
	"$SPEC_TEMPLATE" >"$BUILD_DIR/SPECS/${PACKAGE_NAME}.spec"

# Build SRPM
echo "--- Building SRPM ---"
rpmbuild \
	--define "_topdir $BUILD_DIR" \
	-bs "$BUILD_DIR/SPECS/${PACKAGE_NAME}.spec"

# Copy SRPM to caller's working directory so it survives temp dir cleanup
SRPM="$(find "$BUILD_DIR/SRPMS" -name '*.src.rpm' -type f | head -1)"
if [[ -z "$SRPM" ]]; then
	echo "Error: SRPM not found"
	exit 1
fi

SRPM_OUT="$REPO_ROOT/$(basename "$SRPM")"
cp "$SRPM" "$SRPM_OUT"

echo "=== SRPM built successfully ==="
echo "$SRPM_OUT"
