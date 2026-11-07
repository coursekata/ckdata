# The Rust core identifies recoverable SSO failures and resolves source profiles
# with AWS's parser. This layer owns only R interaction and the AWS CLI process.
connect_handle <- function(workgroup, schema, profile, region, role_arn, login) {
  connect <- function() native_wait(native_connect(workgroup, schema, profile, region, role_arn))
  rlang::try_fetch(connect(), ckdata_sso_login_required = function(cnd) {
    if (!login) {
      rlang::abort(
        c(
          "AWS SSO sign-in is required; automatic sign-in is disabled.",
          i = "Sign in using your preferred AWS method and reconnect, or allow `login = TRUE`."
        ),
        class = "ckdata_sso_login_required", sso_login = cnd$sso_login, parent = cnd
      )
    }
    aws_sso_login(cnd$sso_login)
    # Keep the retry outside this handler: a second authentication failure must
    # propagate instead of opening another browser or retrying indefinitely.
    connect()
  })
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
      rlang::abort("Could not run AWS CLI. Install AWS CLI v2 and restart R.", class = "ckdata_login_tool_missing", parent = cnd)
    }
  )
  if (!length(version) || !is.null(attr(version, "status")) || !grepl("^aws-cli/2[.]", version[[1]])) {
    rlang::abort("Browser sign-in requires AWS CLI v2. Update the installed AWS CLI and restart R.", class = "ckdata_login_tool_version")
  }
  aws
}

aws_sso_login <- function(target) {
  aws <- aws_cli()
  args <- c("sso", "login", "--profile", shQuote(target$profile))
  if (!rlang::is_null(target$session)) args <- c(args, "--sso-session", shQuote(target$session))
  args <- c(args, "--no-cli-pager", "--no-cli-auto-prompt")
  cli::cli_inform("Opening AWS SSO sign-in in your browser. Complete sign-in to continue; interrupt R to cancel.")
  status <- rlang::try_fetch(
    suppressWarnings(system2(aws, args, stdout = "", stderr = "")),
    error = function(cnd) {
      rlang::abort("Could not start AWS CLI sign-in. Check your AWS CLI v2 installation.", class = "ckdata_login_failed", parent = cnd)
    }
  )
  if (status != 0L) {
    rlang::abort(
      "AWS SSO sign-in did not complete. No connection retry was made.",
      class = "ckdata_login_failed", status = status
    )
  }
  invisible(NULL)
}
