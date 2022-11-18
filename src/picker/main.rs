#![allow(non_snake_case)]

pub(crate) mod cli;
pub(crate) mod context;

pub type NeovimConnection = connection::NeovimConnection<connection::Neovim<connection::IoWrite>>;
pub type NeovimBuffer = connection::Buffer<connection::IoWrite>;


#[tokio::main(worker_threads=2)]
async fn main() {
    connection::init_logger();

    let env_ctx = context::gather_env::enter();

    main::warn_if_incompatible_options(&env_ctx.opt);

    connect_neovim(env_ctx).await;
}

mod main {
    // Some options takes effect only when page would be
    // spawned from neovim's terminal
    pub fn warn_if_incompatible_options(opt: &crate::cli::Options) {
        if opt.address.is_some() {
            return
        }

        if opt.is_split_implied() {
            log::warn!(
                target: "usage",
                "Split (-r -l -u -d -R -L -U -D) is ignored \
                if address (-a or $NVIM) isn't set"
            );
        }
        if opt.back || opt.back_restore {
            log::warn!(
                target: "usage",
                "Switch back (-b -B) is ignored \
                if address (-a or $NVIM) isn't set"
            );
        }
    }
}


async fn connect_neovim(env_ctx: context::EnvContext) {
    log::info!(target: "context", "{env_ctx:#?}");

    connection::init_panic_hook();

    let nvim_conn = connection::open(
        &env_ctx.tmp_dir,
        &env_ctx.page_id,
        &env_ctx.opt.address,
        &env_ctx.opt.config,
        &env_ctx.opt.config,
        false
    ).await;

    open_files(env_ctx, nvim_conn).await
}


async fn open_files(env_ctx: context::EnvContext, mut conn: NeovimConnection) {

    if env_ctx.opt.is_split_implied() {
        let cmd = open_files::create_split_command(&env_ctx.opt.split);
        conn.nvim_actions.exec_lua(&cmd, vec![]).await
            .expect("Cannot create split window");
    }

    use context::gather_env::FilesUsage;
    match env_ctx.files_usage {
        FilesUsage::RecursiveCurrentDir { recurse_depth } => {
            let read_dir = walkdir::WalkDir::new("./")
                .contents_first(true)
                .follow_links(false)
                .max_depth(recurse_depth);

            for f in read_dir {
                let f = f.expect("Cannot recursively read dir entry");
                let f = open_files::FileToOpen::new(f.path());

                if !f.is_text && !env_ctx.opt.open_non_text {
                    continue
                }

                open_files::open_file(&mut conn, &env_ctx, &f.path_string).await;
            }
        },
        FilesUsage::LastModifiedFile => {
            let mut last_modified = None;

            let read_dir = std::fs::read_dir("./").expect("Cannot read current directory");
            for f in read_dir {
                let f = f.expect("Cannot read dir entry");
                let f = open_files::FileToOpen::new(f.path());

                if !f.is_text && !env_ctx.opt.open_non_text {
                    continue;
                }

                let f_modified_time = f.get_modified_time();

                if let Some((last_modified_time, last_modified)) = last_modified.as_mut() {
                    if *last_modified_time < f_modified_time {
                        (*last_modified_time, *last_modified) = (f_modified_time, f);
                    }
                } else {
                    last_modified.replace((f_modified_time, f));
                }
            }

            if let Some((_, f)) = last_modified {
                open_files::open_file(&mut conn, &env_ctx, &f.path_string).await;
            }
        },
        FilesUsage::FilesProvided => {
            for f in &env_ctx.opt.files {
                let f = open_files::FileToOpen::new(f.as_str());

                if !f.is_text && !env_ctx.opt.open_non_text {
                    continue
                }

                open_files::open_file(&mut conn, &env_ctx, &f.path_string).await;
            }
        }
    }

    if env_ctx.opt.back || env_ctx.opt.back_restore {
        let (win, buf) = &conn.initial_win_and_buf;
        conn.nvim_actions.set_current_win(win).await
            .expect("Cannot return to initial window");
        conn.nvim_actions.set_current_buf(buf).await
            .expect("Cannot return to initial buffer");

        if env_ctx.opt.back_restore {
            conn.nvim_actions.command("norm! A").await
                .expect("Cannot return to insert mode");
        }
    }
}


mod open_files {
    use std::{path::{PathBuf, Path}, time::SystemTime};
    use crate::context::EnvContext;

    use once_cell::unsync::Lazy;
    const PWD: Lazy<PathBuf> = Lazy::new(|| {
        PathBuf::from(std::env::var("PWD").unwrap())
    });

    pub struct FileToOpen {
        pub path: PathBuf,
        pub path_string: String,
        pub is_text: bool,
    }

    impl FileToOpen {
        pub fn new<P: AsRef<Path>>(path: P) -> FileToOpen {
            let path = PWD.join(path);
            let path_string = path
                .to_string_lossy()
                .to_string();
            let is_text = is_text_file(&path_string);
            FileToOpen {
                path,
                path_string,
                is_text
            }
        }

        pub fn get_modified_time(&self) -> SystemTime {
            let f_meta = self.path
                .metadata()
                .expect("Cannot read dir entry metadata");
            f_meta
                .modified()
                .expect("Cannot read modified metadata")
        }
    }

    pub fn is_text_file(f: &str) -> bool {
        let file_cmd = std::process::Command::new("file")
            .arg(f)
            .output()
            .expect("Cannot get `file` output");
        let file_cmd_output = String::from_utf8(file_cmd.stdout)
            .expect("Non UTF8 `file` output");

        let filetype = file_cmd_output
            .split(": ")
            .last()
            .expect("Wrong `file` output format");

        filetype == "ASCII text\n"
    }


    pub async fn open_file(
        conn: &mut super::NeovimConnection,
        env_ctx: &EnvContext,
        f: &str
    ) {
        let cmd = format!("e {}", f);
        conn.nvim_actions.command(&cmd).await
            .expect("Cannot open file buffer");

        if env_ctx.opt.follow {
            conn.nvim_actions.command("norm! G").await
                .expect("Cannot execute follow command")

        } else if let Some(pattern) = &env_ctx.opt.pattern {
            let cmd = format!("norm! /{pattern}");
            conn.nvim_actions.command(&cmd).await
                .expect("Cannot execute follow command")

        } else if let Some(pattern_backwards) = &env_ctx.opt.pattern_backwards {
            let cmd = format!("norm! ?{pattern_backwards}");
            conn.nvim_actions.command(&cmd).await
                .expect("Cannot execute follow command")
        }

        if env_ctx.opt.keep || env_ctx.opt.keep_until_write {
            let (channel, page_id) = (conn.channel, &env_ctx.page_id);
            let (mut bd, mut ev) = ("", "BufDelete");

            if env_ctx.opt.keep_until_write {
                (bd, ev) = (
                    "vim.api.nvim_buf_delete(buf, { force = true })",
                    "BufWritePost"
                )
            }

            let cmd = indoc::formatdoc! {r#"
                local buf = vim.api.nvim_get_current_buf()
                vim.api.nvim_create_autocmd('{ev}', {{
                    buffer = buf,
                    callback = function()
                        pcall(function()
                            {bd}
                            vim.rpcnotify({channel}, 'page_buffer_closed', '{page_id}')
                        end)
                    end
                }})
            "#};
            conn.nvim_actions.exec_lua(&cmd, vec![]).await
                .expect("Cannot execute keep command");
        }

        if let Some(lua) = &env_ctx.opt.lua {
            conn.nvim_actions.exec_lua(lua, vec![]).await
                .expect("Cannot execute lua command");
        }

        if let Some(command) = &env_ctx.opt.command {
            conn.nvim_actions.command(command).await
                .expect("Cannot execute command")
        }

        if env_ctx.opt.keep || env_ctx.opt.keep_until_write {
            match conn.rx.recv().await {
                _ => return,
            }
        }

    }

    pub fn create_split_command(
        opt: &crate::cli::SplitOptions
    ) -> String {
        if opt.popup {

            let w_ratio = |s| format!("math.floor(((w / 2) * 3) / {})", s + 1);
            let h_ratio = |s| format!("math.floor(((h / 2) * 3) / {})", s + 1);

            let (w, h, o) = ("w".to_string(), "h".to_string(), "0".to_string());

            let (width, height, row, col);

            if opt.split_right != 0 {
                (width = w_ratio(opt.split_right), height = h, row = &o, col = &w)

            } else if opt.split_left != 0 {
                (width = w_ratio(opt.split_left),  height = h, row = &o, col = &o)

            } else if opt.split_below != 0 {
                (width = w, height = h_ratio(opt.split_below), row = &h, col = &o)

            } else if opt.split_above != 0 {
                (width = w, height = h_ratio(opt.split_above), row = &o, col = &o)

            } else if let Some(split_right_cols) = opt.split_right_cols.map(|x| x.to_string()) {
                (width = split_right_cols, height = h, row = &o, col = &w)

            } else if let Some(split_left_cols) = opt.split_left_cols.map(|x| x.to_string()) {
                (width = split_left_cols,  height = h, row = &o, col = &o)

            } else if let Some(split_below_rows) = opt.split_below_rows.map(|x| x.to_string()) {
                (width = w, height = split_below_rows, row = &h, col = &o)

            } else if let Some(split_above_rows) = opt.split_above_rows.map(|x| x.to_string()) {
                (width = w, height = split_above_rows, row = &o, col = &o)

            } else {
                unreachable!()
            };

            indoc::formatdoc! {"
                local w = vim.api.nvim_win_get_width(0)
                local h = vim.api.nvim_win_get_height(0)
                local buf = vim.api.nvim_create_buf(true, false)
                local win = vim.api.nvim_open_win(buf, true, {{
                    relative = 'editor',
                    width = {width},
                    height = {height},
                    row = {row},
                    col = {col}
                }})
                vim.api.nvim_set_current_win(win)
                vim.api.nvim_win_set_option(win, 'winblend', 25)
            "}
        } else {

            let w_ratio = |s| format!("' .. tostring(math.floor(((w / 2) * 3) / {})) .. '", s + 1);
            let h_ratio = |s| format!("' .. tostring(math.floor(((h / 2) * 3) / {})) .. '", s + 1);

            let (a, b) = ("aboveleft", "belowright");
            let (w, h) = ("winfixwidth", "winfixheight");
            let (v, z) = ("vsplit", "split");

            let (direction, size, split, fix);

            if opt.split_right != 0 {
                (direction = b, size = w_ratio(opt.split_right), split = v, fix = w)

            } else if opt.split_left != 0 {
                (direction = a,  size = w_ratio(opt.split_left), split = v, fix = w)

            } else if opt.split_below != 0 {
                (direction = b, size = h_ratio(opt.split_below), split = z, fix = h)

            } else if opt.split_above != 0 {
                (direction = a, size = h_ratio(opt.split_above), split = z, fix = h)

            } else if let Some(split_right_cols) = opt.split_right_cols.map(|x| x.to_string()) {
                (direction = b, size = split_right_cols, split = v, fix = w)

            } else if let Some(split_left_cols) = opt.split_left_cols.map(|x| x.to_string()) {
                (direction = a, size = split_left_cols,  split = v, fix = w)

            } else if let Some(split_below_rows) = opt.split_below_rows.map(|x| x.to_string()) {
                (direction = b, size = split_below_rows, split = z, fix = h)

            } else if let Some(split_above_rows) = opt.split_above_rows.map(|x| x.to_string()) {
                (direction = a, size = split_above_rows, split = z, fix = h)

            } else {
                unreachable!()
            };

            indoc::formatdoc! {"
                local prev_win = vim.api.nvim_get_current_win()
                local w = vim.api.nvim_win_get_width(prev_win)
                local h = vim.api.nvim_win_get_height(prev_win)
                vim.cmd('{direction} {size}{split}')
                local buf = vim.api.nvim_create_buf(true, false)
                vim.api.nvim_set_current_buf(buf)
                local win = vim.api.nvim_get_current_win()
                vim.api.nvim_win_set_option(win, '{fix}', true)
            "}
        }
    }
}