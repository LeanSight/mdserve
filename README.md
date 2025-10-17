# mdserve

Fast markdown preview server with **live reload**, **theme support**, and **directory browsing**.

Just run `mdserve file.md` to preview a file, or `mdserve .` to browse an entire directory. One statically-compiled executable that runs anywhere - no installation, no dependencies.

![Terminal output when starting mdserve](mdserve-terminal-output.png)

## Features

- 📂 **Directory Browsing** - Serve entire directories with file navigation and automatic detection
- ⚡ **Instant Live Reload** - Real-time updates via WebSocket when markdown file changes
- 🎨 **Multiple Themes** - Built-in theme selector with 5 themes including Catppuccin variants
- 📝 **GitHub Flavored Markdown** - Full GFM support including tables, strikethrough, code blocks, and task lists
- 📊 **Mermaid Diagrams** - Automatic rendering of flowcharts, sequence diagrams, class diagrams, and more
- 🔒 **Secure** - Path traversal prevention and hidden file filtering built-in
- 🚀 **Fast** - Built with Rust and Axum for excellent performance and low memory usage

## Installation

### macOS (Homebrew)

```bash
brew install mdserve
```

### Linux

```bash
curl -sSfL https://raw.githubusercontent.com/jfernandez/mdserve/main/install.sh | bash
```

This will automatically detect your platform and install the latest binary to your system.

### Alternative Methods

#### Using Cargo

```bash
cargo install mdserve
```

#### Arch Linux

```bash
sudo pacman -S mdserve
```

#### Nix Package Manager

``` bash
nix profile install github:jfernandez/mdserve
```

#### From Source

```bash
git clone https://github.com/jfernandez/mdserve.git
cd mdserve
cargo build --release
cp target/release/mdserve <folder in your PATH>
```

#### Manual Download

Download the appropriate binary for your platform from the [latest release](https://github.com/jfernandez/mdserve/releases/latest).

## Usage

### Basic Usage

```bash
# Serve a single markdown file on default port (3000)
mdserve README.md

# Serve an entire directory with file navigation
mdserve .
mdserve docs/

# Serve on custom port
mdserve README.md --port 8080
mdserve . -p 8080

# Bind to specific hostname (default: 0.0.0.0 for container compatibility)
mdserve README.md --hostname 127.0.0.1
```

### Directory Mode

When serving a directory, mdserve automatically:
- Lists all markdown and text files with icons (📁 folders, 📄 files)
- Provides breadcrumb navigation for easy browsing
- Applies the same theme system across all views
- Caches rendered files for optimal performance
- Filters hidden files (`.git`, `.env`, etc.) for security

### File Mode

When serving a single file, mdserve provides:
- Live reload when the file changes
- Direct rendering with full GFM support
- Mermaid diagram rendering
- Theme customization

## Endpoints

### File Mode
Once running, the server provides (default: [http://localhost:3000](http://localhost:3000)):

- **[`/`](http://localhost:3000/)** - Rendered HTML with live reload via WebSocket
- **[`/raw`](http://localhost:3000/raw)** - Raw markdown content (useful for debugging)
- **[`/ws`](http://localhost:3000/ws)** - WebSocket endpoint for real-time updates

### Directory Mode
When serving a directory:

- **[`/`](http://localhost:3000/)** - Directory index with file listing
- **[`/view/file.md`](http://localhost:3000/view/file.md)** - Rendered markdown file
- **[`/raw/file.md`](http://localhost:3000/raw/file.md)** - Raw markdown content
- **[`/ws`](http://localhost:3000/ws)** - WebSocket endpoint for real-time updates

## Theme System

**Built-in Theme Selector**
- Click the 🎨 button in the top-right corner to open theme selector
- **5 Available Themes**:
  - **Light**: Clean, bright theme optimized for readability
  - **Dark**: GitHub-inspired dark theme with comfortable contrast
  - **Catppuccin Latte**: Warm light theme with soothing pastels
  - **Catppuccin Macchiato**: Cozy mid-tone theme with rich colors
  - **Catppuccin Mocha**: Deep dark theme with vibrant accents
- **Persistent Preference**: Your theme choice is automatically saved in browser localStorage

*Click the theme button (🎨) to access the built-in theme selector*

![Theme picker interface](mdserve-theme-picker.png)

*mdserve running with the Catppuccin Macchiato theme - notice the warm, cozy colors and excellent readability*

![mdserve with Catppuccin Macchiato theme](mdserve-catppuccin-macchiato.png)

## Development

### Prerequisites

- Rust 1.85+ (2024 edition)

### Building

```bash
cargo build --release
```

### Running Tests

```bash
# Run all tests
cargo test

# Run integration tests only
cargo test --test integration_test
```

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Credits

**Original Author:** [Jose Fernandez](https://github.com/jfernandez) ([@jfernandez](https://github.com/jfernandez))

**Directory Serving & Enhanced Features:** Developed with TDD methodology by [LeanSight](https://github.com/LeanSight) team

## Acknowledgments

- Built with [Axum](https://github.com/tokio-rs/axum) web framework
- Markdown parsing by [markdown-rs](https://github.com/wooorm/markdown-rs)
- Template rendering by [minijinja](https://github.com/mitsuhiko/minijinja)
- [Catppuccin](https://catppuccin.com/) color themes
- Inspired by various markdown preview tools
