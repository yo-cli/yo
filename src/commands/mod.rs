pub mod auto_command;
pub mod clone_command;
pub mod github_init;
pub mod s5_command;

// Re-export
pub use auto_command::AutoCommand;
pub use clone_command::CloneCommand;
pub use github_init::GitHubInitCommand;
pub use s5_command::S5Command;
