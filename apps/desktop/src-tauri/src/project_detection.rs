use serde::Serialize;
use std::fs;
use std::path::Path;

use crate::scanner::{ScanEntry, ScanEntryKind, ScanFileStatus};

const MAX_GRADLE_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_GRADLE_CONFIG_FILES: usize = 16;
const MAX_DETECTION_EVIDENCE: usize = 20;
const MAX_FRAMEWORK_CONFIG_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetection {
    pub kotlin: DetectionResult,
    pub java: DetectionResult,
    pub python: DetectionResult,
    pub spring_boot: DetectionResult,
    pub gradle: DetectionResult,
    pub node: DetectionResult,
    pub react: DetectionResult,
    pub express: DetectionResult,
    pub nest_js: DetectionResult,
    pub fast_api: DetectionResult,
    pub django: DetectionResult,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectionResult {
    pub status: DetectionStatus,
    pub message: String,
    pub evidence: Vec<DetectionEvidence>,
    pub evidence_omitted_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DetectionStatus {
    Detected,
    NotDetected,
    Uncertain,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectionEvidence {
    pub relative_path: String,
    pub description: String,
}

pub fn detect_project(root: &Path, entries: &[ScanEntry]) -> ProjectDetection {
    let kotlin = detect_language(entries, "kt", "Kotlin 소스 파일 확장자");
    let java = detect_language(entries, "java", "Java 소스 파일 확장자");
    let python = detect_language(entries, "py", "Python 소스 파일 확장자");
    let gradle = detect_gradle(entries);
    let spring_boot = detect_spring_boot(root, entries);
    let node = detect_framework(root, entries, Framework::Node);
    let react = detect_framework(root, entries, Framework::React);
    let express = detect_framework(root, entries, Framework::Express);
    let nest_js = detect_framework(root, entries, Framework::NestJs);
    let fast_api = detect_framework(root, entries, Framework::FastApi);
    let django = detect_framework(root, entries, Framework::Django);

    ProjectDetection {
        kotlin,
        java,
        python,
        spring_boot,
        gradle,
        node,
        react,
        express,
        nest_js,
        fast_api,
        django,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Framework {
    Node,
    React,
    Express,
    NestJs,
    FastApi,
    Django,
}

impl Framework {
    fn label(self) -> &'static str {
        match self {
            Self::Node => "Node.js",
            Self::React => "React",
            Self::Express => "Express",
            Self::NestJs => "NestJS",
            Self::FastApi => "FastAPI",
            Self::Django => "Django",
        }
    }

    fn markers(self) -> &'static [&'static str] {
        match self {
            Self::Node => &["\"dependencies\"", "\"devDependencies\""],
            Self::React => &["react", "react-dom"],
            Self::Express => &["express"],
            Self::NestJs => &["@nestjs/core", "@nestjs/common"],
            Self::FastApi => &["fastapi", "from fastapi", "import fastapi"],
            Self::Django => &["django", "from django", "import django"],
        }
    }

    fn candidate(self, path: &str) -> bool {
        let name = file_name(path).to_ascii_lowercase();
        match self {
            Self::Node | Self::React | Self::Express | Self::NestJs => name == "package.json",
            Self::FastApi | Self::Django => matches!(
                name.as_str(),
                "requirements.txt" | "pyproject.toml" | "poetry.lock" | "pipfile" | "pipfile.lock"
            ),
        }
    }
}

fn detect_framework(root: &Path, entries: &[ScanEntry], framework: Framework) -> DetectionResult {
    let candidates = entries
        .iter()
        .filter(|entry| {
            matches!(entry.kind, ScanEntryKind::File) && framework.candidate(&entry.relative_path)
        })
        .collect::<Vec<_>>();
    let mut evidence = Vec::new();
    let mut omitted = 0;
    let mut unreadable = false;
    let mut marker_found = false;
    for entry in candidates
        .iter()
        .filter(|entry| is_eligible(entry))
        .take(MAX_GRADLE_CONFIG_FILES)
    {
        match read_config(root, entry) {
            Some(contents)
                if framework == Framework::Node
                    || contains_any_marker(&contents, framework.markers()) =>
            {
                if framework == Framework::Node
                    || contains_any_marker(&contents, framework.markers())
                {
                    marker_found = true;
                    push_evidence(
                        &mut evidence,
                        &mut omitted,
                        DetectionEvidence {
                            relative_path: entry.relative_path.clone(),
                            description: format!("{} 설정 또는 의존성 단서", framework.label()),
                        },
                    );
                }
            }
            Some(_) => {}
            None => unreadable = true,
        }
    }
    if candidates.iter().any(|entry| !is_eligible(entry)) {
        unreadable = true;
    }
    if marker_found {
        detected(evidence, omitted)
    } else if !candidates.is_empty() || unreadable {
        uncertain(
            evidence,
            omitted,
            &format!("{} 감지 근거를 확정하지 못했습니다.", framework.label()),
        )
    } else {
        not_detected(&format!(
            "{} 감지 근거를 찾지 못했습니다.",
            framework.label()
        ))
    }
}

fn read_config(root: &Path, entry: &ScanEntry) -> Option<String> {
    if !is_eligible(entry)
        || entry.size_bytes.unwrap_or(MAX_FRAMEWORK_CONFIG_BYTES + 1) > MAX_FRAMEWORK_CONFIG_BYTES
    {
        return None;
    }
    let relative = Path::new(&entry.relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    let canonical_root = fs::canonicalize(root).ok()?;
    let path = fs::canonicalize(canonical_root.join(relative)).ok()?;
    if !path.starts_with(&canonical_root) || !fs::metadata(&path).ok()?.is_file() {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn contains_any_marker(contents: &str, markers: &[&str]) -> bool {
    let lower = contents.to_ascii_lowercase();
    markers
        .iter()
        .any(|marker| lower.contains(&marker.to_ascii_lowercase()))
}

fn detect_language(entries: &[ScanEntry], extension: &str, description: &str) -> DetectionResult {
    let candidates = entries
        .iter()
        .filter(|entry| {
            matches!(entry.kind, ScanEntryKind::File)
                && entry.extension.as_deref() == Some(extension)
        })
        .collect::<Vec<_>>();
    let (evidence, evidence_omitted_count) =
        limited_evidence(candidates.iter().map(|entry| DetectionEvidence {
            relative_path: entry.relative_path.clone(),
            description: description.into(),
        }));

    if candidates.iter().any(|entry| is_eligible(entry)) {
        detected(evidence, evidence_omitted_count)
    } else if candidates.is_empty() {
        not_detected("스캔된 파일에서 감지 근거를 찾지 못했습니다.")
    } else {
        uncertain(
            evidence,
            evidence_omitted_count,
            "소스 확장자는 있지만 파일을 확인하지 못해 언어를 확정하지 않았습니다.",
        )
    }
}

fn detect_gradle(entries: &[ScanEntry]) -> DetectionResult {
    let candidates = entries
        .iter()
        .filter(|entry| {
            matches!(entry.kind, ScanEntryKind::File) && is_gradle_file(&entry.relative_path)
        })
        .collect::<Vec<_>>();
    let (evidence, evidence_omitted_count) =
        limited_evidence(candidates.iter().map(|entry| DetectionEvidence {
            relative_path: entry.relative_path.clone(),
            description: "Gradle 빌드 스크립트 또는 래퍼 파일".into(),
        }));

    if candidates.iter().any(|entry| is_eligible(entry)) {
        detected(evidence, evidence_omitted_count)
    } else if candidates.is_empty() {
        not_detected("스캔된 파일에서 감지 근거를 찾지 못했습니다.")
    } else {
        uncertain(
            evidence,
            evidence_omitted_count,
            "Gradle 파일은 있지만 내용을 확인할 수 없어 사용 여부를 확정하지 않았습니다.",
        )
    }
}

fn detect_spring_boot(root: &Path, entries: &[ScanEntry]) -> DetectionResult {
    let app_configs = entries
        .iter()
        .filter(|entry| {
            matches!(entry.kind, ScanEntryKind::File) && is_application_config(&entry.relative_path)
        })
        .collect::<Vec<_>>();
    let gradle_builds = entries
        .iter()
        .filter(|entry| {
            matches!(entry.kind, ScanEntryKind::File)
                && is_gradle_build_file(&entry.relative_path)
                && is_eligible(entry)
        })
        .collect::<Vec<_>>();

    let mut evidence = Vec::new();
    let mut evidence_omitted_count = 0;
    let mut has_boot_marker = false;
    let mut unreadable_build = false;
    let mut inspection_limited = false;
    for (index, entry) in gradle_builds.iter().enumerate() {
        if index >= MAX_GRADLE_CONFIG_FILES {
            inspection_limited = true;
            for skipped in gradle_builds.iter().skip(index) {
                push_evidence(
                    &mut evidence,
                    &mut evidence_omitted_count,
                    DetectionEvidence {
                        relative_path: skipped.relative_path.clone(),
                        description: "Gradle 빌드 파일 (검사 한도로 내용 미확인)".into(),
                    },
                );
            }
            break;
        }
        match read_gradle_config(root, entry) {
            Some(contents) if contains_spring_boot_marker(&contents) => {
                has_boot_marker = true;
                push_evidence(
                    &mut evidence,
                    &mut evidence_omitted_count,
                    DetectionEvidence {
                        relative_path: entry.relative_path.clone(),
                        description: "Gradle 설정에서 Spring Boot 플러그인 또는 starter 선언"
                            .into(),
                    },
                );
            }
            Some(_) => {}
            None => {
                unreadable_build = true;
                push_evidence(
                    &mut evidence,
                    &mut evidence_omitted_count,
                    DetectionEvidence {
                        relative_path: entry.relative_path.clone(),
                        description: "Gradle 빌드 파일 (내용을 확인하지 못함)".into(),
                    },
                );
            }
        }
    }

    for entry in &app_configs {
        push_evidence(
            &mut evidence,
            &mut evidence_omitted_count,
            DetectionEvidence {
                relative_path: entry.relative_path.clone(),
                description: "Spring에서 자주 쓰이는 application 설정 파일".into(),
            },
        );
    }

    if has_boot_marker {
        detected(evidence, evidence_omitted_count)
    } else if !app_configs.is_empty() || unreadable_build || inspection_limited {
        uncertain(
            evidence,
            evidence_omitted_count,
            if inspection_limited {
                "검사 한도 안에서 Spring Boot 선언을 찾지 못해 결과를 확정하지 않았습니다."
            } else {
                "설정 파일은 있지만 Spring Boot를 확정할 빌드 선언을 확인하지 못했습니다."
            },
        )
    } else {
        not_detected("Spring Boot 감지 근거를 찾지 못했습니다.")
    }
}

fn read_gradle_config(root: &Path, entry: &ScanEntry) -> Option<String> {
    if !is_eligible(entry) {
        return None;
    }

    let relative_path = Path::new(&entry.relative_path);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }

    let path = fs::canonicalize(root.join(relative_path)).ok()?;
    if !path.starts_with(root) {
        return None;
    }
    let metadata = fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_GRADLE_CONFIG_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn is_eligible(entry: &ScanEntry) -> bool {
    matches!(entry.file_status, Some(ScanFileStatus::Eligible))
}

fn is_gradle_file(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    matches!(
        name.as_str(),
        "settings.gradle.kts"
            | "build.gradle.kts"
            | "settings.gradle"
            | "build.gradle"
            | "gradlew"
            | "gradlew.bat"
    )
}

fn is_gradle_build_file(path: &str) -> bool {
    matches!(
        file_name(path).to_ascii_lowercase().as_str(),
        "build.gradle.kts" | "build.gradle"
    )
}

fn is_application_config(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    let is_supported_extension = matches!(extension, "yml" | "yaml" | "properties");
    let is_application_name = stem == "application" || stem.starts_with("application-");
    is_supported_extension && is_application_name
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn contains_spring_boot_marker(contents: &str) -> bool {
    let mut uncommented = String::with_capacity(contents.len());
    let chars = contents.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut in_block_comment = false;
    let mut quote = None;
    let mut triple_quote = false;

    while index < chars.len() {
        let current = chars[index];
        let next = chars.get(index + 1).copied();

        if in_block_comment {
            if current == '*' && next == Some('/') {
                in_block_comment = false;
                index += 2;
            } else {
                if current == '\n' {
                    uncommented.push('\n');
                }
                index += 1;
            }
            continue;
        }

        if let Some(delimiter) = quote {
            if current == '\\' {
                uncommented.push(current);
                index += 1;
                if index < chars.len() {
                    uncommented.push(chars[index]);
                    index += 1;
                }
                continue;
            }
            if triple_quote
                && current == delimiter
                && next == Some(delimiter)
                && chars.get(index + 2) == Some(&delimiter)
            {
                uncommented.extend([delimiter, delimiter, delimiter]);
                quote = None;
                triple_quote = false;
                index += 3;
                continue;
            }
            uncommented.push(current);
            if !triple_quote && current == delimiter {
                quote = None;
            }
            index += 1;
            continue;
        }

        if current == '/' && next == Some('/') {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if current == '/' && next == Some('*') {
            in_block_comment = true;
            index += 2;
            continue;
        }
        if current == '#'
            && uncommented
                .rsplit('\n')
                .next()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if current == '"' || current == '\'' {
            quote = Some(current);
            triple_quote = next == Some(current) && chars.get(index + 2) == Some(&current);
            uncommented.push(current);
            index += 1;
            if triple_quote {
                uncommented.push(current);
                uncommented.push(current);
                index += 2;
            }
            continue;
        }

        uncommented.push(current);
        index += 1;
    }

    uncommented.contains("org.springframework.boot") || uncommented.contains("spring-boot-starter")
}

fn detected(evidence: Vec<DetectionEvidence>, evidence_omitted_count: usize) -> DetectionResult {
    DetectionResult {
        status: DetectionStatus::Detected,
        message: "스캔된 파일에서 감지 근거를 찾았습니다.".into(),
        evidence,
        evidence_omitted_count,
    }
}

fn not_detected(message: &str) -> DetectionResult {
    DetectionResult {
        status: DetectionStatus::NotDetected,
        message: message.into(),
        evidence: Vec::new(),
        evidence_omitted_count: 0,
    }
}

fn uncertain(
    evidence: Vec<DetectionEvidence>,
    evidence_omitted_count: usize,
    message: &str,
) -> DetectionResult {
    DetectionResult {
        status: DetectionStatus::Uncertain,
        message: message.into(),
        evidence,
        evidence_omitted_count,
    }
}

fn limited_evidence(
    items: impl IntoIterator<Item = DetectionEvidence>,
) -> (Vec<DetectionEvidence>, usize) {
    let mut evidence = Vec::new();
    let mut omitted_count = 0;
    for item in items {
        push_evidence(&mut evidence, &mut omitted_count, item);
    }
    (evidence, omitted_count)
}

fn push_evidence(
    evidence: &mut Vec<DetectionEvidence>,
    omitted_count: &mut usize,
    item: DetectionEvidence,
) {
    if evidence.len() < MAX_DETECTION_EVIDENCE {
        evidence.push(item);
    } else {
        *omitted_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{ScanEntry, ScanEntryKind, ScanFileStatus};
    use std::path::PathBuf;

    fn entry(path: &str, status: ScanFileStatus) -> ScanEntry {
        let name = file_name(path).to_string();
        ScanEntry {
            relative_path: path.into(),
            name: name.clone(),
            kind: ScanEntryKind::File,
            size_bytes: Some(1),
            modified_unix_ms: None,
            extension: PathBuf::from(name)
                .extension()
                .map(|extension| extension.to_string_lossy().to_ascii_lowercase()),
            file_status: Some(status),
        }
    }

    #[test]
    fn detects_supported_gradle_and_application_config_names() {
        for path in [
            "settings.gradle.kts",
            "build.gradle.kts",
            "settings.gradle",
            "build.gradle",
            "gradlew",
            "gradlew.bat",
        ] {
            assert!(is_gradle_file(path), "expected Gradle evidence for {path}");
        }
        for path in [
            "src/main/resources/application.yml",
            "src/main/resources/application.yaml",
            "src/main/resources/application.properties",
            "src/main/resources/application-dev.yml",
        ] {
            assert!(
                is_application_config(path),
                "expected Spring config evidence for {path}"
            );
        }
        assert!(!is_application_config("application.txt"));
        assert!(!is_application_config("my-application.yml"));
    }

    #[test]
    fn ignores_spring_boot_markers_in_comments() {
        let contents = r#"
            // id("org.springframework.boot")
            /*
             * org.springframework.boot
             */
            # spring-boot-starter-web
            id("java")
        "#;
        assert!(!contains_spring_boot_marker(contents));
        assert!(contains_spring_boot_marker(
            "implementation(\"org.springframework.boot:spring-boot-starter-web\")"
        ));
    }

    #[test]
    fn scans_code_after_inline_block_comments_and_preserves_comment_like_strings() {
        assert!(contains_spring_boot_marker(
            "/* explanatory comment */ id(\"org.springframework.boot\")"
        ));
        assert!(contains_spring_boot_marker(
            "// example /* not a block comment\nid(\"org.springframework.boot\")"
        ));
        assert!(contains_spring_boot_marker(
            "repositories { maven(\"https://repo.example/spring-boot-starter\") }"
        ));
        assert!(!contains_spring_boot_marker("/* comment */\nid(\"java\")"));
    }

    #[test]
    fn distinguishes_missing_and_unreadable_language_evidence() {
        let absent = detect_language(&[], "kt", "Kotlin 소스 파일 확장자");
        assert!(matches!(absent.status, DetectionStatus::NotDetected));

        let unreadable = detect_language(
            &[entry("src/Main.kt", ScanFileStatus::Binary)],
            "kt",
            "Kotlin 소스 파일 확장자",
        );
        assert!(matches!(unreadable.status, DetectionStatus::Uncertain));
    }

    #[test]
    fn limits_evidence_collection_and_reports_omitted_count() {
        let (evidence, omitted_count) = limited_evidence((0..25).map(|index| DetectionEvidence {
            relative_path: format!("src/{index}.kt"),
            description: "Kotlin source".into(),
        }));

        assert_eq!(evidence.len(), MAX_DETECTION_EVIDENCE);
        assert_eq!(omitted_count, 5);
    }

    #[test]
    fn framework_candidates_are_limited_to_supported_config_files() {
        assert!(Framework::React.candidate("package.json"));
        assert!(Framework::FastApi.candidate("pyproject.toml"));
        assert!(Framework::Django.candidate("requirements.txt"));
        assert!(!Framework::React.candidate("src/App.tsx"));
        assert!(!Framework::FastApi.candidate("src/main.py"));
    }

    #[test]
    fn framework_markers_are_case_insensitive() {
        assert!(contains_any_marker(
            "{\"dependencies\": {\"react\": \"18\"}}",
            Framework::React.markers()
        ));
        assert!(contains_any_marker(
            "from FASTAPI import FastAPI",
            Framework::FastApi.markers()
        ));
    }
}
