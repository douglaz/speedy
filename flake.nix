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
        # Default package
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "speedy";
          version = "0.1.0";
          src = ./.;
          
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
          
          nativeBuildInputs = with pkgs; [
            pkg-config
            rustToolchain
            llvmPackages.clang
          ];
          
          buildInputs = with pkgs; [
            ffmpegFull.dev
            # Image processing libraries
            imagemagick
            # System libraries
            openssl
          ];
          
          # Environment variables for FFmpeg
          FFMPEG_DIR = "${ffmpegFull.dev}";
          PKG_CONFIG_PATH = "${ffmpegFull.dev}/lib/pkgconfig";
          
          # Set library paths
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
            ffmpegFull
          ];
          
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
            llvmPackages.clang
            cmake
            
            # Musl tools for static linking
            musl
            pkgsMusl.stdenv.cc
            
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
          
          # Rust environment
          RUST_BACKTRACE = "1";
          RUST_LOG = "info";
          
          shellHook = ''
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