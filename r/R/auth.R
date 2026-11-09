# The Rust core identifies recoverable SSO failures and resolves source profiles
# with AWS's parser. This layer owns only R interaction and the AWS CLI process.
resolve_auth_settings <- function(profile, explicit_profile, sso, supplied) {
  for (name in names(sso)) {
    if (!explicit_profile && !supplied[[name]]) {
      sso[[name]] <- config_env(paste0("CKDATA_SSO_", toupper(name)))
    }
    if (!rlang::is_null(sso[[name]])) {
      single_string(sso[[name]], paste0("sso_", name))
    }
  }
  sso <- purrr::compact(sso)
  if (!length(sso)) {
    sso <- list()
  }
  if (!explicit_profile && length(sso)) {
    sso$fallback_profile <- profile
    profile <- NULL
  }
  list(profile = profile, sso = sso)
}

connect_handle <- function(
  workgroup,
  schema,
  profile,
  region,
  role_arn,
  output_location,
  login,
  sso = list()
) {
  native_wait(native_connect(
    list(
      workgroup = workgroup,
      schema = schema,
      profile = profile,
      region = region,
      role_arn = role_arn,
      output_location = output_location,
      sso = sso
    ),
    login
  ))
}

aws_cli <- function() {
  aws <- unname(Sys.which("aws"))
  if (!nzchar(aws)) {
    rlang::abort(
      c(
        "Browser sign-in requires AWS CLI v2, but `aws` was not found.",
        i = "Install AWS CLI v2 and restart R, or authenticate separately and use `login = FALSE`."
      ),
      class = "ckdata_login_tool_missing"
    )
  }
  version <- rlang::try_fetch(
    suppressWarnings(system2(aws, "--version", stdout = TRUE, stderr = TRUE)),
    error = function(cnd) {
      rlang::abort(
        "Could not run AWS CLI. Install AWS CLI v2 and restart R.",
        class = "ckdata_login_tool_missing",
        parent = cnd
      )
    }
  )
  if (
    !length(version) ||
      !is.null(attr(version, "status")) ||
      !grepl("^aws-cli/2[.]", version[[1]])
  ) {
    rlang::abort(
      "Browser sign-in requires AWS CLI v2. Update the installed AWS CLI and restart R.",
      class = "ckdata_login_tool_version"
    )
  }
  aws
}

aws_sso_login <- function(target, job = NULL) {
  aws <- aws_cli()
  args <- c("sso", "login", "--profile", target$profile)
  if (!rlang::is_null(target$session)) {
    args <- c(args, "--sso-session", target$session)
  }
  args <- c(args, "--no-cli-pager", "--no-cli-auto-prompt")
  cli::cli_inform(
    "Opening AWS SSO sign-in in your browser. Complete sign-in to continue; interrupt R to cancel."
  )
  env <- NULL
  if (!rlang::is_null(target$config)) {
    directory <- fs::file_temp("ckdata-sso-")
    fs::dir_create(directory, mode = "u=rwx")
    withr::defer(fs::dir_delete(directory))
    config <- fs::path(directory, "config")
    writeLines(target$config, config)
    fs::file_chmod(config, "u=rw")
    env <- c("current", AWS_CONFIG_FILE = config)
  }
  status <- rlang::try_fetch(
    run_aws_cli(aws, args, env, job),
    error = function(cnd) {
      rlang::abort(
        "Could not complete AWS CLI sign-in.",
        class = "ckdata_login_failed",
        parent = cnd
      )
    }
  )
  if (is.na(status)) {
    return(invisible(FALSE))
  }
  if (status != 0L) {
    rlang::abort(
      "AWS SSO sign-in did not complete. No connection retry was made.",
      class = "ckdata_login_failed",
      status = status
    )
  }
  invisible(NULL)
}

# processx passes arguments and environment directly to the child on all supported
# platforms. It never changes the R process's AWS configuration.
start_aws_cli <- function(aws, args, env) {
  processx::process$new(
    aws,
    args,
    env = env,
    stdout = "",
    stderr = "",
    cleanup_tree = TRUE
  )
}

run_aws_cli <- function(aws, args, env, job = NULL) {
  process <- start_aws_cli(aws, args, env)
  withr::defer(if (process$is_alive()) process$kill_tree())
  while (process$is_alive()) {
    if (!rlang::is_null(job) && native_ready(job)) {
      return(NA_integer_)
    }
    Sys.sleep(0.05)
  }
  process$get_exit_status()
}
