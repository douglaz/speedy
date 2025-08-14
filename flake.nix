{
  description = "speedy - Video processing tool for speed adjustment, LUT application, and color enhancement";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        
        # Use pkgsMusl for static linking
        pkgsMusl = pkgs.pkgsMusl;
        
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
          targets = [ "x86_64-unknown-linux-musl" "aarch64-unknown-linux-musl" ];
        };

        # FFmpeg with all features for video processing
        ffmpegFull = pkgs.ffmpeg_7-full.override {
          withNvcodec = true;  # NVIDIA hardware acceleration
          withVaapi = true;    # VAAPI hardware acceleration
          withVdpau = true;    # VDPAU hardware acceleration
          withX264 = true;     # H.264 encoder
          withX265 = true;     # H.265/HEVC encoder
          withVpx = true;      # VP8/VP9 encoder
          withAom = true;      # AV1 encoder
        };
      in
      {
        # Default package: static musl build
        packages.default = let
          rustPlatformMusl = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
        in rustPlatformMusl.buildRustPackage {
          pname = "speedy";
          version = "0.1.0";
          src = ./.;
          
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
          
          nativeBuildInputs = with pkgs; [
            pkg-config
            rustToolchain
            pkgsStatic.stdenv.cc
          ];
          
          # Note: FFmpeg is called as external command, not linked
          # So we don't need it as a build input for the static binary
          
          # Force cargo to use the musl target
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgs.pkgsStatic.stdenv.cc}/bin/${pkgs.pkgsStatic.stdenv.cc.targetPrefix}cc";
          CC_x86_64_unknown_linux_musl = "${pkgs.pkgsStatic.stdenv.cc}/bin/${pkgs.pkgsStatic.stdenv.cc.targetPrefix}cc";
          CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=-static";
          
          # Override buildPhase to use the correct target
          buildPhase = ''
            runHook preBuild
            
            echo "Building with musl target for static binary..."
            cargo build \
              --release \
              --target x86_64-unknown-linux-musl \
              --offline \
              -j $NIX_BUILD_CORES \
              --workspace
            
            runHook postBuild
          '';
          
          installPhase = ''
            runHook preInstall
            
            mkdir -p $out/bin
            cp target/x86_64-unknown-linux-musl/release/speedy $out/bin/
            
            runHook postInstall
          '';
          
          # Ensure static linking
          doCheck = false; # Tests don't work well with static linking
          
          # Verify the binary is statically linked
          postInstall = ''
            echo "Checking if binary is statically linked..."
            file $out/bin/speedy
            # Strip the binary to reduce size
            ${pkgs.binutils}/bin/strip $out/bin/speedy
          '';
          
          meta = with pkgs.lib; {
            description = "Video processing tool for speed adjustment, LUT application, and color enhancement";
            homepage = "https://github.com/yourusername/speedy";
            license = licenses.mit;
            maintainers = [ ];
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            bashInteractive
            rustToolchain
            
            # Build tools
            pkg-config
            pkgsStatic.stdenv.cc
            
            # FFmpeg and video processing
            ffmpegFull
            ffmpegFull.dev
            
            # Image processing
            imagemagick
            
            # Development tools
            rust-analyzer
            cargo-watch
            cargo-edit
            cargo-outdated
            
            # Git tools
            gh
            git
            
            # System libraries
            openssl
            openssl.dev
            
            # Optional: video analysis tools
            mediainfo
            mkvtoolnix
          ];

          # Environment variables
          FFMPEG_DIR = "${ffmpegFull.dev}";
          PKG_CONFIG_PATH = "${ffmpegFull.dev}/lib/pkgconfig:${pkgs.openssl.dev}/lib/pkgconfig";
          
          # Library paths
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
            ffmpegFull
            pkgs.openssl
          ];
          
          # For musl target compilation
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgs.pkgsStatic.stdenv.cc}/bin/${pkgs.pkgsStatic.stdenv.cc.targetPrefix}cc";
          CC_x86_64_unknown_linux_musl = "${pkgs.pkgsStatic.stdenv.cc}/bin/${pkgs.pkgsStatic.stdenv.cc.targetPrefix}cc";
          
          # Rust environment
          RUST_BACKTRACE = "1";
          RUST_LOG = "info";
          
          shellHook = ''
            # Set up Git hooks if not already configured
            if [ -d .git ] && [ -d .githooks ]; then
              current_hooks_path=$(git config core.hooksPath || echo "")
              if [ "$current_hooks_path" != ".githooks" ]; then
                echo "📎 Setting up Git hooks for code quality checks..."
                git config core.hooksPath .githooks
                echo "✅ Git hooks configured automatically!"
                echo "   • pre-commit: Checks code formatting"
                echo "   • pre-push: Runs formatting and clippy checks"
                echo ""
                echo "To disable: git config --unset core.hooksPath"
                echo ""
              fi
            fi
            
            echo "Welcome to Speedy development environment!"
            echo ""
            echo "FFmpeg version:"
            ${ffmpegFull}/bin/ffmpeg -version | head -n1
            echo ""
            echo "Available commands:"
            echo "  cargo build        - Build the project"
            echo "  cargo run          - Run the application"
            echo "  cargo test         - Run tests"
            echo "  cargo watch        - Watch for changes and rebuild"
            echo ""
            echo "Example usage:"
            echo "  cargo run -- -i input.mp4 -o output.mp4 --speed 2.0 --lut color_grade.cube"
            echo ""
            
            # Create sample directories if they don't exist
            mkdir -p samples/input samples/output samples/luts
            
            echo "Sample directories created:"
            echo "  samples/input/  - Place input videos here"
            echo "  samples/output/ - Processed videos will be saved here"
            echo "  samples/luts/   - Place LUT files (.cube, .3dl) here"
          '';
        };
      }
    );
}