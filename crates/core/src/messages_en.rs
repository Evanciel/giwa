//! 사용자 대면 문장의 영어판 (ADR-0134). 키와 `{name}` 자리는 `messages::lookup` 과 같다.

/// 키에 해당하는 영어 문장.
pub(crate) fn lookup(key: &str) -> Option<&'static str> {
    let text = match key {
        "error.internal" => "An internal error occurred. Check the logs with the request ID.",
        "error.usage" => "The command usage is not correct.",
        "error.validation" => "The input value is not valid.",
        "error.not_found" => "Not found.",
        "error.unauthorized" => "Authentication is required, or the credentials are not valid.",
        "error.forbidden" => "You do not have permission to do this.",
        "error.conflict" => "It already exists or conflicts with the current state.",
        "error.port_in_use" => "Port {port} is in use by another program.",
        "error.schema_too_new" => "The database is newer than this version of maru.",
        "error.rate_limited" => "Too many requests. Try again shortly.",
        "error.external" => "An external connection failed.",
        "error.service" => "Could not register the service: {detail}",
        "hint.port_in_use" => "Run on a different port.",
        "error.usage_detail" => "The command usage is not correct: {detail}",
        "hint.usage" => "Run `maru --help` to see the usage.",
        "hint.schema_too_new" => "Run a newer maru or restore from a backup.",
        "error.io" => "Could not read or write the file: {path}",
        "error.config_parse" => "Could not parse the config file ({path}): {detail}",
        "error.config_site_url" => "site_url must start with http:// or https://: {value}",
        "error.config_bind" => "bind must look like `127.0.0.1:8080`: {value}",
        "error.data_dir_not_initialized" => "The data directory is not initialized: {path}",
        "error.forbidden_permission" => "This requires the `{permission}` permission.",
        "error.forbidden_board" => "This token cannot do `{permission}` on this board.",
        "error.field_length" => "{field} is empty or longer than {max} characters.",
        "error.slug_invalid" => {
            "A slug may contain only letters, numbers and `-` (80 characters at most): {value}"
        }
        "error.slug_taken" => "Board {board} already has the slug `{slug}`.",
        "error.board_kind" => "Board kind `{value}` is not supported. Allowed: {allowed}",
        "error.board_layout" => {
            "Board layout `{value}` is not supported. Allowed: {allowed} (empty means the default for the kind)"
        }
        "error.board_order" => {
            "List order `{value}` is not supported. Allowed: {allowed} (empty means the default for the kind)"
        }
        "error.post_locked" => "This post is locked. Only admins can comment, reply or edit.",
        "error.accept_kind" => "Accepting an answer works only on qna and forum boards.",
        "error.accept_who" => "Only the author or a board admin can accept an answer.",
        "error.accept_comment" => "Only a public comment on this post can be accepted: {id}",
        "error.board_policy_role" => "Unknown role `{value}` in the board policy.",
        "error.board_exists" => "Board `{slug}` already exists.",
        "error.board_not_found" => "Board `{slug}` was not found.",
        "error.post_not_found" => "Post not found: {id}",
        "error.field_count" => "You can create up to {max} extra fields.",
        "error.field_key" => {
            "Field key \"{value}\" cannot be used. Start with a lowercase letter and use lowercase letters, numbers and _, up to 32 characters."
        }
        "error.field_dup" => "The same key ({key}) appears twice.",
        "error.field_type" => "Field {key} cannot use the type {value}. Allowed: {allowed}",
        "error.field_label" => "Give field {key} a name of {max} characters or fewer.",
        "error.field_options" => {
            "Check the options of field {key}: 1 to 50 options, each non-empty, unique and up to 100 characters."
        }
        "error.field_unknown" => "This board has no such field: {key}",
        "error.field_required" => "Please fill in {label}.",
        "error.field_value" => "The value of {label} is not valid ({hint}).",
        "error.field_arg" => "--field takes key=value: {value}",
        "error.field_json" => "Could not read the extra field JSON: {detail}",
        "error.reply_closed" => "This board does not accept reply posts.",
        "error.reply_depth" => "Replies can go {max} levels deep at most.",
        "error.revision_not_found" => "Not a revision of this post: {id}",
        "error.bulk_size" => "You can choose 1 to {max} items at a time.",
        "error.bulk_action" => {
            "Bulk action `{value}` is not supported. Allowed: publish, hide, delete, restore, move"
        }
        "error.bulk_board" => "Choose a board to move to.",
        "error.post_transition" => "A post in `{from}` cannot change to `{to}`.",
        "error.body_too_large" => "The body is too large ({max} bytes at most).",
        "error.timestamp" => "The time must be in RFC3339 format: {value}",
        "error.schedule_range" => "The scheduled time must be within {days} days from now: {value}",
        "error.schedule_pick" => "Choose a date and time to schedule.",
        "error.post_schedule_status" => "A post in `{status}` cannot be scheduled.",
        "hint.post_schedule_status" => "Hide a published post before scheduling it.",
        "error.cover_url" => {
            "The cover image must be a photo uploaded to this site (/media/...): {value}"
        }
        "error.token_invalid" => "The token is missing, expired or revoked.",
        "error.token_not_found" => "Token `{id}` was not found.",
        "error.post_template_builtin" => {
            "The built-in template `{name}` cannot be edited or deleted. Save it under another name."
        }
        "error.post_template_not_found" => "Template `{name}` was not found.",
        "error.category_not_found" => "Board {board} has no category `{slug}`.",
        "error.category_exists" => "Board {board} already has the category `{slug}`.",
        "error.category_too_many" => "A board can have up to {max} categories.",
        "error.tag_length" => "A tag can have {max} characters at most: {value}",
        "error.tag_invalid" => "A tag needs at least one letter or number: {value}",
        "error.tag_too_many" => "A post can have up to {max} tags.",
        "error.tag_not_found" => "Tag `{slug}` was not found.",
        "error.post_secret" => "This post is private. Only the author and admins can read it.",
        "error.post_views" => "The view count must be an integer from 0 to {max}.",
        "hint.post_secret" => "If you are the author, log in and open it again.",
        "error.search_query" => {
            "The search is empty, has only exclusions (-), or exceeds {max} characters or 10 words: {query}"
        }
        "error.search_option" => "Search option `{name}` cannot be `{value}`. Allowed: {allowed}",
        "hint.search_query" => {
            "Examples: `soup Jeju`, `Jeju OR Busan`, `\"trail 7\"`, `soup -Busan`"
        }
        "error.signup_closed" => "This site is not accepting sign-ups right now.",
        "error.name_invalid" => {
            "A nickname must be {min} to {max} characters, without control characters or @ < > \" \\: {value}"
        }
        "error.name_reserved" => "`{value}` cannot be used as a nickname.",
        "error.bio_invalid" => {
            "A bio can have {max} characters at most, without control characters."
        }
        "error.signature_invalid" => {
            "A signature can have {max} characters and {lines} lines at most, without control characters."
        }
        "error.board_description" => {
            "A board description is plain text of {max} characters at most, without control characters."
        }
        "error.field_role" => {
            "Field `{key}` cannot have the role `{value}`. url fields can be sponsored or source, date fields can be deadline, and each role fits one field."
        }
        "error.field_filter" => {
            "Field `{key}`: filter works only on select, multiselect and boolean fields."
        }
        "error.name_taken" => "The nickname `{value}` is already taken.",
        "error.email_unverified" => {
            "Your email is not verified yet. We sent the verification email again, so check your inbox."
        }
        "error.ticket_invalid" => "The link has expired or was already used. Request a new one.",
        "error.password_wrong" => "The current password is not correct.",
        "error.owner_withdraw" => "The owner account cannot leave the site.",
        "error.withdraw_name" => "To confirm, enter your current nickname exactly.",
        "error.email_taken_external" => {
            "This email is already registered. Log in with that account and connect social login in your account."
        }
        "error.member_self" => "You cannot change the status or role of your own account here.",
        "error.member_owner" => "The status and role of the owner account cannot be changed.",
        "error.member_withdrawn" => "This member has left the site.",
        "error.member_role" => "The role `{value}` cannot be given. Allowed: {allowed}",
        "error.mail_not_found" => "Mail `{id}` was not found.",
        "error.mail_from" => "The sender address `{value}` does not look like an email address.",
        "error.mail_smtp_missing" => {
            "With transport smtp, [mail.smtp] needs host, port and security."
        }
        "error.mail_smtp_host" => "The SMTP server name `{value}` is not valid.",
        "error.mail_smtp_port" => {
            "The SMTP port is empty. It is usually 587 (starttls) or 465 (tls)."
        }
        "error.mail_smtp_plain" => {
            "Unencrypted SMTP (security = none) works only with a relay on this computer: {host}"
        }
        "error.link_not_found" => {
            "No link card for `{url}`. Open a post containing it once (or run `maru link list`) and try again."
        }
        "error.link_status" => "There is no link status `{value}`. Allowed: pending, ok, failed",
        "error.mail_test_failed" => "Could not send the test mail: {detail}",
        "error.mail_transport" => "There is no mail transport `{value}`. Allowed: file, smtp",
        "error.mail_security" => {
            "There is no SMTP security mode `{value}`. Allowed: starttls, tls, none"
        }
        "error.mail_to" => "The recipient address `{value}` does not look like an email address.",
        "mail.test.subject" => "[{site}] Test mail",
        "mail.test.body" => {
            "This is a test mail from {site}. If you received it, the mail settings are right.\n\nSign-up verification and password reset mails go out the same way.\n"
        }
        "error.mail_local_only" => {
            "Mail bodies contain one-time links, so they can be read only with `maru mail` on this computer."
        }
        "error.banned_word" => "It contains the banned word \"{word}\" and cannot be saved.",
        "error.banned_word_long" => "A banned word can have {max} characters at most.",
        "error.banned_words_many" => "You can have up to {max} banned words.",
        "error.flood" => "You are writing too quickly. Try again in {seconds} seconds.",
        "error.flood_seconds" => "The writing interval is 0 to 3600 seconds: {value}",
        "error.same_comment" => "This is the same as your previous comment.",
        "error.comments_closed" => "This board does not accept comments.",
        "error.comment_agent" => {
            "Agent tokens do not write comments. They can only approve, hide or mark as spam."
        }
        "error.comment_not_found" => "Comment not found: {id}",
        "error.comment_deleted" => "A deleted comment cannot be restored.",
        "error.cloudflared_missing" => {
            "cloudflared is not installed. Install it as the official guide says and run this again: {url}"
        }
        "error.expose_no_server" => {
            "The maru server is not answering at {target}. Start the server first."
        }
        "error.expose_failed" => "Could not open the tunnel: {detail}",
        "error.expose_hostname" => "The domain name is not valid: {value}",
        "error.expose_local" => {
            "Only the owner on this computer can publish the site. A token cannot do this."
        }
        "error.mcp_token" => "The MCP server needs a token. It does not run with owner rights.",
        "error.comment_length" => "A comment must be 1 to {max} characters.",
        "error.comment_image" => {
            "Comments can attach only images uploaded to this site (jpg, png, gif, webp)."
        }
        "error.comment_password" => "The password is not correct.",
        "error.post_password" => "The password is not correct.",
        "error.guest_secret" => "Guests cannot write private posts. Please log in.",
        "error.post_agent_board" => {
            "Agent tokens do not use the board writing screen. Write a draft and publish it with the publish permission."
        }
        "error.guest_required" => "If you are not logged in, enter a name and a password.",
        "error.guest_password" => {
            "Choose a password of {min} characters or more. It is used to edit or delete later."
        }
        "error.captcha_wrong" => "The answer to the check question is not correct.",
        "error.captcha_option" => {
            "The check question mode `{value}` cannot be used. Allowed: {allowed}"
        }
        "error.comment_policy" => "The approval mode `{value}` cannot be used. Allowed: {allowed}",
        "error.signup_option" => "The sign-up mode `{value}` cannot be used. Allowed: {allowed}",
        "error.skin_unknown" => "There is no skin `{value}`. Allowed: {allowed}",
        "mail.verify.subject" => "[{site}] Please confirm your email address",
        "mail.verify.body" => {
            "{name}, thank you for signing up at {site}.\n\nOpen the link below to finish signing up. It works once, within 24 hours.\n\n{link}\n\nIf you did not sign up, you can delete this mail.\n"
        }
        "mail.exists.subject" => "[{site}] This email address is already registered",
        "mail.exists.body" => {
            "Someone tried to sign up at {site} with this email address. It is already registered.\n\nIf you forgot your password, set a new one with the link below. It works once, within 1 hour.\n\n{link}\n\nIf this was not you, you can delete this mail.\n"
        }
        "mail.dormant_notice.subject" => "[{site}] Your account becomes dormant in 30 days",
        "mail.dormant_notice.body" => {
            "{name}, you have not logged in to {site} for a long time.\n\nAccounts with no login for {days} days become dormant, and their email address is stored separately. Log in within 30 days to keep using it as is. Even after it becomes dormant, logging in brings it back right away.\n\n{link}\n"
        }
        "mail.signals.subject" => "[{site}] {count} things to check",
        "mail.signals.body" => {
            "{name}, there is something to check on the site.\n\n{lines}\n\nAdmin: {admin}\n\nTurn off operation signal mail in the site settings to stop these mails.\n"
        }
        "mail.reset.subject" => "[{site}] Reset your password",
        "mail.reset.body" => {
            "{name}, open the link below to reset your password. It works once, within 1 hour.\n\n{link}\n\nIf you did not request this, you can delete this mail. Your password stays the same.\n"
        }
        "error.file_not_found" => "Attachment `{id}` was not found.",
        "error.file_type" => "This file cannot be attached: {name} (allowed: {allowed})",
        "error.file_content" => "The file content does not match its extension ({ext}): {name}",
        "error.file_name" => "The file name is empty ({max} characters at most).",
        "error.media_not_found" => "Media `{id}` was not found.",
        "error.media_type" => "Only image files (jpg, png, gif, webp) can be uploaded: {name}",
        "error.media_too_large" => "The file is too large. Upload {max}MB or less.",
        "error.not_wiki" => "Board \"{board}\" is not a wiki. Change the board kind to wiki.",
        "error.media_decode" => {
            "Could not read the image. The file is broken or too large: {detail}"
        }
        "error.media_rotate" => "Rotation can be only 0, 90, 180 or 270 degrees (got {value}).",
        "error.media_crop_arg" => "--crop takes `x,y,width,height`, four integers of 0 or more.",
        "error.media_edit_empty" => "Give an angle to rotate or an area to crop.",
        "error.media_crop" => {
            "The crop area goes outside the photo ({width}×{height}), measured after rotation."
        }
        "error.template_not_found" => "Template not found: {name}",
        "error.json_body" => "The request body JSON is not valid: {detail}",
        "error.post_status" => "Unknown post status: {value}",
        "error.preset_unknown" => "Unknown preset: {value}",
        "error.starter_unknown" => "Unknown page starter: {value} (choose from: {allowed})",
        "error.starter_invalid" => "Could not read page starters: {detail}",
        "error.draft_screen" => "Invalid screen name for a draft: {value}",
        "error.draft_human_only" => {
            "Only signed-in people save drafts. Agents can only read a screen's drafts."
        }
        "error.draft_body" => "A draft body must be a JSON object.",
        "error.saved_blocks_full" => {
            "You can keep up to {max} saved blocks. Delete unused ones and try again."
        }
        "error.draft_too_large" => {
            "The draft is too large (max {max}KB). Remove some blocks or publish first."
        }
        "error.board_not_empty" => "Board `{slug}` has {count} posts and cannot be deleted.",
        "error.page_exists" => "Page `{slug}` already exists.",
        "error.page_not_found" => "Page `{slug}` was not found.",
        "error.page_parent_cycle" => "`{slug}` is below this page and cannot be its parent.",
        "error.page_parent_deep" => "Pages can go {max} levels deep at most.",
        "error.redirect_from" => {
            "`{path}` cannot be redirected. It must be a path on this site, and not an admin or API address."
        }
        "error.redirect_to" => {
            "Cannot redirect to `{path}`. It must be a path starting with `/` or an http(s) address."
        }
        "error.redirect_loop" => "Redirecting to `{path}` would make a loop.",
        "error.redirect_not_found" => "There is no redirect for `{path}`.",
        "error.page_layout" => "Layout `{value}` is not supported. Allowed: {allowed}",
        "error.menu_location" => "Menu location `{value}` is not supported. Allowed: {allowed}",
        "error.menu_depth" => "Menus can go {max} levels deep at most.",
        "error.changeset_preview_token" => {
            "The preview address has expired or was already used. Get a new one."
        }
        "error.changeset_required" => {
            "A token cannot do `{action}` directly. Put it in a changeset and get a person's approval."
        }
        "error.changeset_not_found" => "Changeset {id} was not found.",
        "error.changeset_status" => "Changeset {id} is {status} and cannot do this.",
        "error.changeset_op" => "The change operation is not valid: {detail}",
        "error.changeset_title" => "A changeset title must be 1 to 200 characters.",
        "error.changeset_failed" => "Stopped at operation {index}: {detail}",
        "error.changeset_apply_agent" => {
            "Agent tokens cannot apply a changeset. Have a person approve it or use an auto-apply token."
        }
        "error.menu_too_many" => "A menu can have up to {max} items.",
        "error.widget_kind" => "Unknown widget kind: {value} (allowed: {allowed})",
        "error.widget_region" => "Unknown region: {value} (allowed: {allowed})",
        "error.widget_scope" => {
            "The widget scope must be all, home, boards, board:<address> or page:<address>: {value}"
        }
        "error.widget_config" => "The `{kind}` widget settings are not valid: {reason}",
        "error.widget_device" => "Devices to hide on are {allowed}: {value}",
        "error.widget_hidden_everywhere" => {
            "To hide a widget on every device, turn it off instead (`maru widget configure <id> --disable`)."
        }
        "error.audit_actor_type" => "The actor type must be one of {allowed} (got {value}).",
        "error.audit_name" => {
            "{field} takes only lowercase letters, numbers, `.`, `_` and `-` (got {value})."
        }
        "error.audit_time" => {
            "{field} is an RFC3339 time (for example 2026-11-04T09:00:00+09:00, got {value})."
        }
        "error.board_upload_role" => {
            "The upload role must be member or above, or none: {value}. Guest uploads are not accepted."
        }
        "error.upload_quota" => "You can upload up to {max} files a day. Try again tomorrow.",
        "error.module_message" => "{text}",
        "error.module_not_found" => "Module not found: {id}",
        "error.module_disabled" => "The module is not enabled: {id}",
        "error.module_tool_not_found" => "Module {module} has no such tool: {tool}",
        "error.module_remove_confirm" => {
            "Removing deletes the data of module {id}. Add --purge --yes. To keep the data, use disable."
        }
        "error.form_number" => "Enter a number in `{field}`: {value}",
        "error.form_choice" => "`{value}` cannot be chosen in `{field}`.",
        "error.menu_child_first" => "Sub-item \"{label}\" needs a top-level item above it.",
        "error.restore_confirm" => {
            "Restoring moves the current data aside and replaces it with the backup. If you are sure, run it again with --yes."
        }
        "error.sample_clear_confirm" => {
            "This permanently deletes the sample posts you have not edited, with their pictures. Edited posts stay. If you are sure, run it again with --yes."
        }
        "error.backup_not_found" => "There is no backup file `{name}`.",
        "error.backup_invalid" => {
            "This is not a Maru backup file, or it is broken: {path} ({detail})"
        }
        "error.backup_newer" => {
            "This backup was made by a newer Maru ({version}, schema {schema}). This binary knows up to schema {known}. Upgrade Maru and restore."
        }
        "error.server_running" => {
            "A server is already running with this data directory ({path}). Stop it and run this again."
        }
        "error.stats_days" => "The stats range is 1 to {max} days.",
        "error.stats_retention" => "Stats retention is 30 to 3650 days (got {value}).",
        "error.dormant_days" => "The dormant period is 0 (off) or 180 to 3650 days (got {value}).",
        "error.mail_retention" => "Mail retention is 7 to 3650 days (got {value}).",
        "error.signal_burst" => {
            "The comment burst threshold is 0 (off) or 3 to 1000 (got {value})."
        }
        "error.share_image" => {
            "The share image is a site image path starting with /media/ or an https:// address (got {value}). Leave it empty to use the picture drawn in the site color."
        }
        "signal.comment_burst" => {
            "Comments are piling up: {count} comments on \"{title}\" in the last hour"
        }
        "error.widget_popup_region" => {
            "Popups go only in the popup region, and the popup region takes only popups (kind {kind}, region {region})."
        }
        "error.widget_not_found" => "Widget not found: {id}",
        "error.widget_too_many" => "You can place up to {max} widgets.",
        "error.menu_url" => "A menu address must start with https://, http:// or /: {value}",
        "error.menu_item" => "The menu item format is not valid: {value}",
        "error.token_value" => "The value of design token `{field}` is not valid: {value}",
        "error.owner_exists" => "An owner account already exists. Get a new login link.",
        "error.member_email_required" => "There are several members, so give an email.",
        "error.member_not_found" => "Member not found.",
        "error.member_disabled" => "This member is disabled.",
        "error.link_invalid" => "The login link has expired or was already used. Get a new one.",
        "error.login_failed" => "The email or password is not correct.",
        "error.totp_code" => {
            "The code is not correct. Enter the current code from the app or a recovery code."
        }
        "error.totp_code_left" => "The code is not correct. You have {left} more tries.",
        "error.totp_locked" => "Too many wrong codes. Log in again from the start.",
        "error.totp_expired" => "The verification step has ended. Log in again from the start.",
        "error.totp_already" => "Two-step verification is already on.",
        "error.totp_not_started" => "Start the two-step verification setup first.",
        "error.totp_off" => "Two-step verification is off.",
        "error.password_length" => "A password must be {min} to {max} characters.",
        "error.email" => "The email format is not valid: {value}",
        "error.csrf" => "The request could not be verified. Refresh the page and try again.",
        "error.setting_verification" => {
            "`{field}` takes letters, numbers, `-` and `_`, up to 100 characters: {value}"
        }
        "error.token_contrast" => "Not enough color contrast: {pair} = {ratio} (minimum {min})",
        "error.preset_invalid" => "Could not read preset `{id}`: {detail}",
        "error.setting_lang" => "The language tag is not valid: {value}",
        "error.setting_tz" => "The time zone must be between -720 and 840 minutes: {value}",
        "error.scope_unknown" => {
            "Unknown scope: {value} (read, draft, publish, structure, design, admin)"
        }
        "error.font_not_found" => "Font `{slug}` was not found.",
        "error.font_file_not_found" => "Font `{slug}` has no file for weight {weight}.",
        "error.font_file" => "This is not a font file, or it is broken. Allowed: {allowed}",
        "error.font_weight" => "The weight must be a multiple of 100 from 100 to 900: {value}",
        "error.font_name" => "A font name must be 1 to {max} characters.",
        "error.font_slug" => {
            "A font address name must start with a lowercase letter, use lowercase letters, numbers and `-`, be 2 to 32 characters and not be a built-in font name: {value}"
        }
        "error.font_fallback" => "The fallback family is not valid: {value} (allowed: {allowed})",
        "error.font_license" => "The license is not valid: {value} (allowed: {allowed})",
        "error.font_license_text" => "The license text must be {max}KB or less.",
        "error.font_rights" => "Confirm that you have the right to serve this font on the web.",
        "error.font_in_use" => {
            "Font `{slug}` is used by design token `{field}` and cannot be deleted."
        }
        "warn.font_large" => {
            "The font file is {size}MB. It can slow down the site for visitors, so reduce it to woff2 (scripts/subset-fonts.py)."
        }
        "error.input_encoding" => "The body is not UTF-8: {path}",
        "hint.input_encoding" => "Save it as UTF-8 in your editor and pass it again with `--from`.",
        _ => return None,
    };
    Some(text)
}
