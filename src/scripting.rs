//! 脚本宏库（端口自 jterm2）。仅作为独立模块保留，未接入 UI/快捷键，
//! 与 jterm2 现状一致（该模块在 jterm2 中同样未被引用）。
#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 脚本类型
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ScriptType {
    Shell,  // bash/sh 脚本
    Lua,    // Lua 脚本
    Python, // Python 脚本
}

/// 脚本宏（快捷键宏）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptMacro {
    pub name: String,
    pub description: String,
    pub script_type: ScriptType,
    pub content: String,
    pub hotkey: Option<String>,
}

/// 脚本库
#[derive(Clone, Debug, Default)]
pub struct ScriptLibrary {
    macros: HashMap<String, ScriptMacro>,
    script_dir: PathBuf,
}

impl ScriptLibrary {
    pub fn new(script_dir: PathBuf) -> Self {
        ScriptLibrary {
            macros: HashMap::new(),
            script_dir,
        }
    }

    /// 注册一个宏
    pub fn register_macro(&mut self, macro_def: ScriptMacro) {
        self.macros.insert(macro_def.name.clone(), macro_def);
    }

    /// 执行宏
    pub fn execute_macro(&self, name: &str) -> Result<String, String> {
        let macro_def = self
            .macros
            .get(name)
            .ok_or_else(|| format!("Macro '{}' not found", name))?;

        self.execute_script(&macro_def.script_type, &macro_def.content)
    }

    /// 执行脚本
    pub fn execute_script(
        &self,
        script_type: &ScriptType,
        content: &str,
    ) -> Result<String, String> {
        use std::process::Command;

        match script_type {
            ScriptType::Shell => {
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(content)
                    .output()
                    .map_err(|e| format!("Failed to execute shell script: {}", e))?;

                if output.status.success() {
                    Ok(String::from_utf8_lossy(&output.stdout).to_string())
                } else {
                    Err(String::from_utf8_lossy(&output.stderr).to_string())
                }
            }
            ScriptType::Lua => {
                // Lua 支持需要额外的 lua 依赖，这里仅作演示
                Err("Lua support requires additional dependency".to_string())
            }
            ScriptType::Python => {
                // Python 支持通过调用 python 命令
                let output = Command::new("python3")
                    .arg("-c")
                    .arg(content)
                    .output()
                    .map_err(|e| format!("Failed to execute Python script: {}", e))?;

                if output.status.success() {
                    Ok(String::from_utf8_lossy(&output.stdout).to_string())
                } else {
                    Err(String::from_utf8_lossy(&output.stderr).to_string())
                }
            }
        }
    }

    /// 获取所有宏
    pub fn list_macros(&self) -> Vec<(String, String)> {
        self.macros
            .iter()
            .map(|(name, macro_def)| (name.clone(), macro_def.description.clone()))
            .collect()
    }

    /// 加载脚本目录
    pub fn load_scripts_from_dir(&mut self) -> Result<usize, String> {
        if !self.script_dir.exists() {
            return Ok(0);
        }

        let mut count = 0;

        if let Ok(entries) = std::fs::read_dir(&self.script_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "toml").unwrap_or(false) {
                    // 尝试解析 TOML 脚本配置
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(macro_def) = toml::from_str::<ScriptMacro>(&content) {
                            self.register_macro(macro_def);
                            count += 1;
                        }
                    }
                }
            }
        }

        Ok(count)
    }

    /// 保存宏到文件
    pub fn save_macro(&self, macro_def: &ScriptMacro) -> Result<(), String> {
        std::fs::create_dir_all(&self.script_dir)
            .map_err(|e| format!("Failed to create script directory: {}", e))?;

        let file_path = self.script_dir.join(format!("{}.toml", macro_def.name));
        let content = toml::to_string_pretty(&macro_def)
            .map_err(|e| format!("Failed to serialize macro: {}", e))?;

        std::fs::write(&file_path, content)
            .map_err(|e| format!("Failed to write macro file: {}", e))?;

        Ok(())
    }
}

/// 事件钩子系统
#[derive(Clone, Debug, Default)]
pub struct EventHooks {
    on_session_created: Vec<String>,  // 会话创建时执行的脚本
    on_command_executed: Vec<String>, // 命令执行时执行的脚本
    on_exit: Vec<String>,             // 退出时执行的脚本
}

impl EventHooks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_on_session_created(&mut self, scripts: Vec<String>) {
        self.on_session_created = scripts;
    }

    pub fn set_on_command_executed(&mut self, scripts: Vec<String>) {
        self.on_command_executed = scripts;
    }

    pub fn set_on_exit(&mut self, scripts: Vec<String>) {
        self.on_exit = scripts;
    }

    pub fn trigger_session_created(&self, library: &ScriptLibrary) -> Result<(), String> {
        for script_name in &self.on_session_created {
            library.execute_macro(script_name)?;
        }
        Ok(())
    }

    pub fn trigger_command_executed(&self, library: &ScriptLibrary) -> Result<(), String> {
        for script_name in &self.on_command_executed {
            library.execute_macro(script_name)?;
        }
        Ok(())
    }

    pub fn trigger_exit(&self, library: &ScriptLibrary) -> Result<(), String> {
        for script_name in &self.on_exit {
            library.execute_macro(script_name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macro_registration() {
        let mut library = ScriptLibrary::new(PathBuf::from("/tmp"));
        let macro_def = ScriptMacro {
            name: "test".to_string(),
            description: "Test macro".to_string(),
            script_type: ScriptType::Shell,
            content: "echo 'hello'".to_string(),
            hotkey: None,
        };

        library.register_macro(macro_def);
        let macros = library.list_macros();
        assert_eq!(macros.len(), 1);
    }

    #[test]
    fn test_shell_execution() {
        let library = ScriptLibrary::new(PathBuf::from("/tmp"));
        let result = library.execute_script(&ScriptType::Shell, "echo 'test'");
        assert!(result.is_ok());
    }
}
