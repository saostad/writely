#![allow(dead_code)] // Wired up in a later round (local-model task).

use std::path::PathBuf;

use crate::config::LocalModelId;

pub struct ModelFile {
    pub id: LocalModelId,
    pub label: &'static str,
    pub file_name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub server_args: &'static [&'static str],
}

static MODEL_0_8B: ModelFile = ModelFile {
    id: LocalModelId::Qwen35_0_8B,
    label: "Qwen3.5 0.8B — fast",
    file_name: "Qwen3.5-0.8B-Q4_K_M.gguf",
    url: "https://huggingface.co/unsloth/Qwen3.5-0.8B-GGUF/resolve/6ab461498e2023f6e3c1baea90a8f0fe38ab64d0/Qwen3.5-0.8B-Q4_K_M.gguf",
    sha256: "bd258782e35f7f458f8aced1adc053e6e92e89bc735ba3be89d38a06121dc517",
    size: 532517120,
    server_args: &[],
};

static MODEL_2B: ModelFile = ModelFile {
    id: LocalModelId::Qwen35_2B,
    label: "Qwen3.5 2B — better quality, slower",
    file_name: "Qwen3.5-2B-MTP-Q4_K_M.gguf",
    url: "https://huggingface.co/unsloth/Qwen3.5-2B-MTP-GGUF/resolve/e05864f8066d874d5f85aaff007ae57a2a7d1efe/Qwen3.5-2B-Q4_K_M.gguf",
    sha256: "18c92228fc1498a02506f64593f7cc8cc02f406f732a217c6ba99ad1254d4f0b",
    size: 1329851808,
    server_args: &["--spec-type", "draft-mtp"],
};

pub fn model_file(id: LocalModelId) -> &'static ModelFile {
    match id {
        LocalModelId::Qwen35_0_8B => &MODEL_0_8B,
        LocalModelId::Qwen35_2B => &MODEL_2B,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    TarGz,
    Zip,
}

pub struct RuntimeFile {
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub kind: ArchiveKind,
    pub server_path: &'static str,
}

pub const RUNTIME_VERSION: &str = "b11900";

static RUNTIME_MACOS_ARM64: RuntimeFile = RuntimeFile {
    version: "b11900",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b11900/llama-b11900-bin-macos-arm64.tar.gz",
    sha256: "6d3af8649e687b288395ee79f4423a21376892edb94520fa6320bfd3eb81700b",
    size: 16295973,
    kind: ArchiveKind::TarGz,
    server_path: "llama-b11900/llama-server",
};

static RUNTIME_MACOS_X64: RuntimeFile = RuntimeFile {
    version: "b11900",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b11900/llama-b11900-bin-macos-x64.tar.gz",
    sha256: "81f27390c9338d017e775eee2b8ee0825cee88a67397e1fbfe615f76a0549877",
    size: 12038400,
    kind: ArchiveKind::TarGz,
    server_path: "llama-b11900/llama-server",
};

static RUNTIME_WINDOWS_X64: RuntimeFile = RuntimeFile {
    version: "b11900",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b11900/llama-b11900-bin-win-cpu-x64.zip",
    sha256: "d3bbbbb7d1b4979b72052fd73cdeeb5a4d5a4e6a3f52aeb40abd6f116558e323",
    size: 24228419,
    kind: ArchiveKind::Zip,
    server_path: "llama-server.exe",
};

static RUNTIME_LINUX_X64: RuntimeFile = RuntimeFile {
    version: "b11900",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b11900/llama-b11900-bin-ubuntu-x64.tar.gz",
    sha256: "3effcc3009af64d6f30c217827d7982b3934577d282feb243b11fb6789ed124e",
    size: 22497562,
    kind: ArchiveKind::TarGz,
    server_path: "llama-b11900/llama-server",
};

pub fn runtime_file() -> Result<&'static RuntimeFile, String> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Ok(&RUNTIME_MACOS_ARM64)
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Ok(&RUNTIME_MACOS_X64)
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Ok(&RUNTIME_WINDOWS_X64)
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Ok(&RUNTIME_LINUX_X64)
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        Err("Running a model on this computer isn't available for this system yet. Choose Gemini, Together AI or Ollama in Settings.".to_string())
    }
}

pub fn local_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            if !local.is_empty() {
                return PathBuf::from(local).join("Writely").join("local");
            }
        }
        if let Ok(home) = std::env::var("USERPROFILE") {
            if !home.is_empty() {
                return PathBuf::from(home)
                    .join("AppData")
                    .join("Local")
                    .join("Writely")
                    .join("local");
            }
        }
        PathBuf::from("Writely").join("local")
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Writely")
            .join("local")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("writely").join("local");
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("writely")
            .join("local")
    }
}

pub fn models_dir() -> PathBuf {
    local_data_dir().join("models")
}

pub fn runtime_dir() -> PathBuf {
    local_data_dir().join("runtime").join(RUNTIME_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_lower_hex64(s: &str) -> bool {
        s.len() == 64
            && s.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    }

    #[test]
    fn every_sha256_is_64_lowercase_hex() {
        for m in [
            model_file(LocalModelId::Qwen35_0_8B),
            model_file(LocalModelId::Qwen35_2B),
        ] {
            assert!(is_lower_hex64(m.sha256), "model {}", m.file_name);
        }
        for r in [
            &RUNTIME_MACOS_ARM64,
            &RUNTIME_MACOS_X64,
            &RUNTIME_WINDOWS_X64,
            &RUNTIME_LINUX_X64,
        ] {
            assert!(is_lower_hex64(r.sha256), "runtime {}", r.url);
            assert_eq!(r.version, "b11900");
        }
    }

    #[test]
    fn models_have_distinct_file_names_and_expected_args() {
        let small = model_file(LocalModelId::Qwen35_0_8B);
        let big = model_file(LocalModelId::Qwen35_2B);
        assert_ne!(small.file_name, big.file_name);
        assert_eq!(small.id, LocalModelId::Qwen35_0_8B);
        assert_eq!(big.id, LocalModelId::Qwen35_2B);
        assert!(small.server_args.is_empty());
        assert_eq!(big.server_args, &["--spec-type", "draft-mtp"]);
    }

    #[test]
    fn runtime_file_on_this_machine() {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let r = runtime_file().expect("linux x86_64 runtime");
            assert_eq!(r.kind, ArchiveKind::TarGz);
            assert_eq!(r.version, "b11900");
            assert_eq!(r.server_path, "llama-b11900/llama-server");
        }
    }

    #[test]
    fn data_dirs_end_with_expected_parts() {
        assert!(models_dir().ends_with("models"));
        assert!(runtime_dir().ends_with("b11900"));
        assert!(local_data_dir().ends_with("local"));
    }
}
