;;; typst-canvas.el --- Live Typst preview in canvas images -*- lexical-binding: t -*-

;;; Commentary:

;; Requires Emacs 32 with a window system, and the module `typst-canvas-dyn'.
;;
;; `typst-canvas-mode' in a Typst buffer shows a live preview in another window.  A Rust thread
;; compiles the buffer text and renders the pages.  When a result is ready, the thread writes to a
;; pipe process.  The process filter copies the changed pages into canvas images, one per page.
;; Diagnostics go to Flymake.  While point is in an equation, the equation shows rendered below
;; its source line.  `typst-canvas-present' shows the pages as slides.

;;; Code:

(require 'cl-lib)
(require 'color)
(require 'flymake)
(require 'pulse)
(require 'typst-canvas-dyn)

(defgroup typst-canvas nil
  "Live Typst preview in canvas images."
  :group 'tools)

(defcustom typst-canvas-zoom-step 1.25
  "Zoom factor of `typst-canvas-zoom-in' and `typst-canvas-zoom-out'."
  :type 'number)

(defcustom typst-canvas-zoom-range '(0.25 . 4.0)
  "Smallest and largest zoom.  Zoom 1 fits the page width to the window."
  :type '(cons number number))

(defcustom typst-canvas-default-width 800
  "Preview width in pixels, if no graphical window shows the preview."
  :type 'natnum)

(defcustom typst-canvas-display-action
  '((display-buffer-reuse-window display-buffer-in-direction)
    (direction . right)
    (window-width . 0.5))
  "Action that `typst-canvas-mode' uses to show the preview buffer.
See `display-buffer'."
  :type 'sexp)

(defcustom typst-canvas-enable-flymake t
  "If non-nil, `typst-canvas-mode' turns on `flymake-mode' to show diagnostics."
  :type 'boolean)

(defcustom typst-canvas-desk-color nil
  "Color around the pages, or nil to derive it from the `default' face background."
  :type '(choice (const :tag "From the default face" nil) color))

(defcustom typst-canvas-follow-cursor t
  "If non-nil, the preview shows the source cursor as a caret.
It scrolls to keep the caret in view.  The caret is hidden when the
cursor is not in text that is on a page, e.g. in code."
  :type 'boolean)

(defconst typst-canvas--follow-delay 0.1
  "Seconds that point must stay still before the caret moves.")

(defcustom typst-canvas-match-theme t
  "If non-nil, pages have the colors of the `default' face.
Documents that set their own page or text colors keep them.  Toggle it
in a preview with `typst-canvas-toggle-theme'."
  :type 'boolean)

(defcustom typst-canvas-inline-math t
  "If non-nil, the equation at point shows rendered below its source line.
It is cut out of the last good compile, so it has the document's styles.
While the text does not compile, it shows dimmed."
  :type 'boolean)

(defcustom typst-canvas-inline-math-scale 1.25
  "Size of the equation below its source line, relative to the buffer text.
1 gives the equation's font the size of the `default' face.  Math fonts
have smaller lowercase letters than most code fonts, so the default is
larger."
  :type 'number)

(defcustom typst-canvas-present-frame t
  "If non-nil, `typst-canvas-present' shows slides in a new fullscreen frame.
Otherwise, it shows them in the selected window, alone in its frame."
  :type 'boolean)

(defconst typst-canvas--present-frame-parameters
  '((name . "typst-canvas presentation")
    (fullscreen . fullboth)
    (menu-bar-lines . 0)
    (tool-bar-lines . 0)
    (tab-bar-lines . 0)
    (vertical-scroll-bars . nil)
    (horizontal-scroll-bars . nil)
    (left-fringe . 8)
    (right-fringe . 8)
    (internal-border-width . 0)
    (background-color . "black")
    (foreground-color . "gray60")
    (unsplittable . t))
  "Frame parameters of a presentation frame.
The right fringe holds the end of the slide line.  Without it, Emacs keeps
the last text column for the end of the line, and cuts the slide.  The
left fringe keeps the slide centered.  Both are black.")

(defconst typst-canvas--present-aspect (/ 9.0 16)
  "Height to width ratio of a presentation without a graphical window.")

(defconst typst-canvas--fallback-text-size 16
  "Font size of the `default' face in pixels, if no graphical frame shows it.")

(defconst typst-canvas--fallback-desk "#808080"
  "Desk color if the `default' face has no known background, e.g. in batch mode.")

(defconst typst-canvas--desk-lightness-shift 8
  "Percent by which the desk lightness differs from the `default' background.
Without it, pages that match the theme would not stand out from the desk.")

;;;; Errors in handlers

(defvar typst-canvas--last-error nil
  "The last error that `typst-canvas--with-guard' reported.")

(defmacro typst-canvas--with-guard (&rest body)
  "Run BODY.  Report an error in it in the echo area, once per distinct error.
Use it in code that Emacs runs outside of commands: process filters,
timers and hooks.  After an error in a process filter, Emacs pauses.
After an error in some hooks, it removes the function from the hook.
And a timer reports each of its errors."
  (declare (indent 0) (debug t))
  `(condition-case-unless-debug err
       (progn ,@body)
     (error (typst-canvas--report-error err))))

(defun typst-canvas--report-error (err)
  "Show ERR in the echo area, unless it was the last error shown."
  (unless (equal err typst-canvas--last-error)
    (setq typst-canvas--last-error err)
    (message "typst-canvas: %s" (error-message-string err))))

;;;; State of the source buffer

(defvar-local typst-canvas--session nil "Module session that compiles this buffer.")
(defvar-local typst-canvas--process nil "Pipe process through which the session notifies.")
(defvar-local typst-canvas--preview nil "Preview buffer.")
(defvar-local typst-canvas--text-timer nil "Timer that sends the changed text.")
(defvar-local typst-canvas--sent 0 "ID of the newest request.")
(defvar-local typst-canvas--status nil "Newest result of `typst-canvas--session-status'.")
(defvar-local typst-canvas--report-fn nil "Newest Flymake report function.")
(defvar-local typst-canvas--reported nil "Diagnostics last sent to Flymake.")
(defvar-local typst-canvas--started-flymake nil "Non-nil if this mode turned on Flymake.")
(defvar-local typst-canvas--caret-timer nil "Timer that moves the caret.")
(defvar-local typst-canvas--caret-point nil "Point that the caret shows, or nil.")
(defvar-local typst-canvas--updated-point nil "Point at the last update of caret and equation.")
(defvar-local typst-canvas--equation-overlay nil "Overlay that shows the equation at point.")
(defvar-local typst-canvas--equation-canvas nil "Canvas image spec of the equation at point.")
(defvar-local typst-canvas--presentation nil "Presentation buffer, or nil.")
(defvar-local typst-canvas--warned-raw-bytes nil
  "Non-nil if the user was told that this buffer has raw bytes.")

(defconst typst-canvas--source-variables
  '(typst-canvas--session typst-canvas--process typst-canvas--preview typst-canvas--text-timer
    typst-canvas--sent typst-canvas--status typst-canvas--report-fn typst-canvas--reported
    typst-canvas--started-flymake typst-canvas--caret-timer typst-canvas--caret-point
    typst-canvas--updated-point typst-canvas--equation-overlay typst-canvas--equation-canvas
    typst-canvas--presentation typst-canvas--warned-raw-bytes)
  "The state of a source buffer, above.
A clone of the buffer gets copies, and must forget them: the session,
process, preview, timers and overlay belong to the original.")

;;;; State of the preview buffer

(defvar-local typst-canvas--source nil "Source buffer of this preview.")
(defvar-local typst-canvas--canvases [] "Canvas image specs, one per page.")
(defvar-local typst-canvas--serials []
  "Serial of the image last copied into each canvas, or nil.")
(defvar-local typst-canvas--zoom 1.0 "Zoom factor.  1 fits the page width to the window.")
(defvar-local typst-canvas--width nil "Window width of the newest request, in pixels.")
(defvar-local typst-canvas--caret-page nil "Page of the caret (0-based), or nil.")

;;;; State of the presentation buffer

(defvar-local typst-canvas--slide 0 "Page that the presentation shows (0-based).")
(defvar-local typst-canvas--slide-canvas nil "Canvas image spec of the slide.")
(defvar-local typst-canvas--slide-serial nil "Serial of the slide image in the canvas, or nil.")
(defvar-local typst-canvas--slide-size nil "(WIDTH . HEIGHT) of the newest slide request.")
(defvar-local typst-canvas--slide-digits "" "Digits typed for `typst-canvas-present-goto'.")
(defvar-local typst-canvas--present-frame nil "Frame made for the presentation, or nil.")
(defvar-local typst-canvas--present-windows nil
  "Window configuration from before the presentation, or nil.")

;;;; Source buffer

;;;###autoload
(define-minor-mode typst-canvas-mode
  "Show a live preview of the current Typst buffer."
  :lighter " Canvas"
  (if typst-canvas-mode
      (condition-case err
          (typst-canvas--start)
        (error
         (typst-canvas--stop)
         (setq typst-canvas-mode nil)
         (signal (car err) (cdr err))))
    (typst-canvas--stop)))

(defun typst-canvas--start ()
  "Start a session for the current buffer, and show its preview."
  (setq typst-canvas--last-error nil)
  (let* ((source (current-buffer))
         (main (or buffer-file-name (expand-file-name "untitled.typ")))
         (root (file-name-directory main))
         (preview (get-buffer-create (format "*typst-canvas: %s*" (buffer-name)))))
    (with-current-buffer preview
      (typst-canvas-preview-mode)
      (setq typst-canvas--source source))
    (setq typst-canvas--preview preview)
    (setq typst-canvas--process
          (make-pipe-process :name (format "typst-canvas: %s" (buffer-name))
                             :noquery t
                             :coding 'binary
                             :filter (lambda (_process _output)
                                       (typst-canvas--with-guard
                                         (typst-canvas--on-notify source)))))
    (setq typst-canvas--session (typst-canvas--session-start root main typst-canvas--process))
    (add-hook 'after-change-functions #'typst-canvas--on-change nil t)
    (add-hook 'post-command-hook #'typst-canvas--on-post-command nil t)
    (add-hook 'kill-buffer-hook #'typst-canvas--on-kill nil t)
    (add-hook 'flymake-diagnostic-functions #'typst-canvas-flymake nil t)
    ;; A new major mode kills the local state, but not the session, process, preview and timers.
    (add-hook 'change-major-mode-hook #'typst-canvas--on-change-major-mode nil t)
    (add-hook 'clone-buffer-hook #'typst-canvas--on-clone nil t)
    (add-hook 'clone-indirect-buffer-hook #'typst-canvas--on-clone nil t)
    (when (and typst-canvas-enable-flymake (not flymake-mode))
      (setq typst-canvas--started-flymake t)
      (flymake-mode 1))
    (typst-canvas--update-theme-hooks)
    (display-buffer preview typst-canvas-display-action)
    (typst-canvas--send-text)))

(defun typst-canvas--stop ()
  "Stop the session of the current buffer, and kill its preview."
  (typst-canvas--remove-hooks)
  (when typst-canvas--text-timer
    (cancel-timer typst-canvas--text-timer)
    (setq typst-canvas--text-timer nil))
  (when typst-canvas--caret-timer
    (cancel-timer typst-canvas--caret-timer)
    (setq typst-canvas--caret-timer nil))
  (setq typst-canvas--caret-point nil
        typst-canvas--updated-point nil)
  (typst-canvas--hide-equation)
  (setq typst-canvas--equation-canvas nil)
  (when (buffer-live-p typst-canvas--presentation)
    (kill-buffer typst-canvas--presentation))
  ;; Stop the thread before deleting the process: in batch mode, Emacs does not ignore SIGPIPE,
  ;; so a write to a deleted pipe kills Emacs.
  (when typst-canvas--session
    (typst-canvas--session-stop typst-canvas--session)
    (setq typst-canvas--session nil))
  (when typst-canvas--process
    (delete-process typst-canvas--process)
    (setq typst-canvas--process nil))
  (when typst-canvas--started-flymake
    (setq typst-canvas--started-flymake nil)
    (flymake-mode -1))
  (setq typst-canvas--report-fn nil
        typst-canvas--reported nil
        typst-canvas--status nil
        typst-canvas--sent 0)
  (typst-canvas--update-theme-hooks)
  (let ((preview typst-canvas--preview))
    (setq typst-canvas--preview nil)
    (when (buffer-live-p preview)
      (kill-buffer preview))))

(defun typst-canvas--remove-hooks ()
  "Remove the local hooks of `typst-canvas-mode' from the current buffer."
  (remove-hook 'after-change-functions #'typst-canvas--on-change t)
  (remove-hook 'post-command-hook #'typst-canvas--on-post-command t)
  (remove-hook 'kill-buffer-hook #'typst-canvas--on-kill t)
  (remove-hook 'flymake-diagnostic-functions #'typst-canvas-flymake t)
  (remove-hook 'change-major-mode-hook #'typst-canvas--on-change-major-mode t)
  (remove-hook 'clone-buffer-hook #'typst-canvas--on-clone t)
  (remove-hook 'clone-indirect-buffer-hook #'typst-canvas--on-clone t))

(defun typst-canvas--on-change-major-mode ()
  "Turn off `typst-canvas-mode' before a new major mode kills its state."
  (typst-canvas--with-guard
    (typst-canvas-mode -1)))

(defun typst-canvas--on-clone ()
  "Forget the state that a clone copied from its source buffer.
The original keeps its session.  An indirect clone shares the text with
the original, so its changes go to the session of the original."
  (typst-canvas--with-guard
    (typst-canvas--remove-hooks)
    (mapc #'kill-local-variable typst-canvas--source-variables)
    (kill-local-variable 'typst-canvas-mode)
    ;; Not `delq': the clone can share the list with the original.
    (setq local-minor-modes (remq 'typst-canvas-mode local-minor-modes))
    (when (buffer-base-buffer)
      (add-hook 'after-change-functions #'typst-canvas--on-change nil t))))

(defun typst-canvas--on-kill ()
  "Stop the session of the killed buffer.  An error must not stop the kill."
  (typst-canvas--with-guard
    (typst-canvas--stop)))

(defun typst-canvas--on-change (&rest _)
  "Send the buffer text after the current command or timer.
One send covers all the changes of a command, e.g. of `replace-regexp'.
In an indirect buffer without a session, send the text of the base
buffer, which changed too."
  (typst-canvas--with-guard
    (let ((buffer (or (and (not typst-canvas--session) (buffer-base-buffer))
                      (current-buffer))))
      (with-current-buffer buffer
        (when (and typst-canvas--session (not typst-canvas--text-timer))
          (setq typst-canvas--text-timer
                (run-with-timer 0 nil #'typst-canvas--send-text-of buffer)))))))

(defun typst-canvas--send-text-of (buffer)
  "Send the text of BUFFER, if it still has a session."
  (typst-canvas--with-guard
    (when (buffer-live-p buffer)
      (with-current-buffer buffer
        (setq typst-canvas--text-timer nil)
        (when typst-canvas--session
          (typst-canvas--send-text))))))

(defun typst-canvas--send-text ()
  "Send the whole text of the current buffer to its session."
  (typst-canvas--request (typst-canvas--unicode-text
                          (save-restriction
                            (widen)
                            (buffer-substring-no-properties (point-min) (point-max))))))

(defconst typst-canvas--raw-byte-regexp "[\x3fff80-\x3fffff]"
  "Regexp that matches a raw byte: a char that is not Unicode, e.g. from invalid UTF-8.")

(defun typst-canvas--unicode-text (text)
  "Return TEXT with each raw byte as U+FFFD.  The module takes only Unicode text.
Raw bytes come from invalid UTF-8, and from unibyte buffers.  Each U+FFFD
is one char, like the raw byte, so positions stay the same.  Tell the
user once per buffer."
  ;; A unibyte string has bytes.  As multibyte, those over 127 are raw bytes.
  (let ((text (string-to-multibyte text)))
    (if (not (string-match-p typst-canvas--raw-byte-regexp text))
        text
      (unless typst-canvas--warned-raw-bytes
        (setq typst-canvas--warned-raw-bytes t)
        (message "typst-canvas: %s has raw bytes, which the preview shows as U+FFFD"
                 (buffer-name)))
      (replace-regexp-in-string typst-canvas--raw-byte-regexp "\ufffd" text t t))))

(defun typst-canvas--request (text)
  "Send TEXT and the preview view to the session of the current buffer.
TEXT nil only re-renders the last good document, or compiles it again
if the theme colors changed.  While a presentation shows, ask for its
slide too."
  (let ((width typst-canvas-default-width)
        (zoom 1.0)
        (match-theme typst-canvas-match-theme)
        (slide (when (buffer-live-p typst-canvas--presentation)
                 (with-current-buffer typst-canvas--presentation
                   (typst-canvas--slide-view)))))
    (when (buffer-live-p typst-canvas--preview)
      (with-current-buffer typst-canvas--preview
        (setq typst-canvas--width (typst-canvas--window-width))
        (setq width typst-canvas--width
              zoom typst-canvas--zoom
              ;; `typst-canvas-toggle-theme' sets it locally in the preview.
              match-theme typst-canvas-match-theme)))
    (pcase-let ((`(,desk ,page ,ink) (typst-canvas--colors match-theme)))
      (setq typst-canvas--sent
            (typst-canvas--session-request typst-canvas--session text width zoom
                                           desk page ink slide)))
    (force-mode-line-update t)))

(defun typst-canvas--colors (match-theme)
  "Return (DESK PAGE INK) as #xRRGGBB, from the `default' face.
PAGE and INK are the page and text colors, or nil if MATCH-THEME is nil
or the face colors are unknown."
  (let* ((background (face-background 'default nil t))
         (foreground (face-foreground 'default nil t))
         (known (and (color-defined-p background) (color-defined-p foreground)))
         (desk (cond (typst-canvas-desk-color)
                     (known (typst-canvas--shift-lightness background))
                     (t typst-canvas--fallback-desk))))
    (list (typst-canvas--color-value desk)
          (and match-theme known (typst-canvas--color-value background))
          (and match-theme known (typst-canvas--color-value foreground)))))

(defun typst-canvas--shift-lightness (color)
  "Return COLOR darkened if it is light, or lightened if it is dark."
  (if (color-dark-p (color-name-to-rgb color))
      (color-lighten-name color typst-canvas--desk-lightness-shift)
    (color-darken-name color typst-canvas--desk-lightness-shift)))

(defun typst-canvas--color-value (color)
  "Return COLOR as #xRRGGBB.  Unknown colors give the fallback desk color."
  ;; Parse "#RRGGBB" without a frame: a text terminal frame would round it to a terminal color.
  (let ((values (or (color-values-from-color-spec color)
                    (color-values color)
                    (color-values-from-color-spec typst-canvas--fallback-desk))))
    ;; Each value is 16-bit.
    (cl-reduce (lambda (pixel value) (logior (ash pixel 8) (ash value -8)))
               values :initial-value 0)))

(defun typst-canvas--update-theme-hooks ()
  "Watch theme changes while any buffer has a session."
  (if (seq-some (lambda (buffer) (buffer-local-value 'typst-canvas--session buffer))
                (buffer-list))
      (progn
        (add-hook 'enable-theme-functions #'typst-canvas--on-theme-change)
        (add-hook 'disable-theme-functions #'typst-canvas--on-theme-change))
    (remove-hook 'enable-theme-functions #'typst-canvas--on-theme-change)
    (remove-hook 'disable-theme-functions #'typst-canvas--on-theme-change)))

(defun typst-canvas--on-theme-change (&rest _)
  "Send the new theme colors of all sessions."
  (typst-canvas--with-guard
    (dolist (buffer (buffer-list))
      (with-current-buffer buffer
        (when typst-canvas--session
          (typst-canvas--request nil))))))

(defun typst-canvas--on-notify (source)
  "Show the newest result of the session of SOURCE."
  (when (buffer-live-p source)
    (with-current-buffer source
      (when typst-canvas--session
        (setq typst-canvas--status (typst-canvas--session-status typst-canvas--session))
        (when (buffer-live-p typst-canvas--preview)
          (let ((session typst-canvas--session)
                (pages (nth 1 typst-canvas--status)))
            (with-current-buffer typst-canvas--preview
              (typst-canvas--show-pages session pages))))
        (when (buffer-live-p typst-canvas--presentation)
          (typst-canvas--show-slide))
        ;; The pages changed, so the caret and the equation can be elsewhere.
        (typst-canvas--update-caret)
        (typst-canvas--report-diagnostics)
        (force-mode-line-update t)))))

;;;; Flymake

(defun typst-canvas-flymake (report-fn &rest _)
  "Flymake backend that reports the diagnostics of the newest compile.
The session compiles on its own.  New diagnostics go to REPORT-FN when
they arrive."
  (setq typst-canvas--report-fn report-fn)
  (typst-canvas--report-diagnostics 'always))

;; The backend only reports what the session found.  The user started the session, and so the
;; compile, by turning on `typst-canvas-mode'.
(function-put 'typst-canvas-flymake 'flymake-always-safe t)

(defun typst-canvas--report-diagnostics (&optional always)
  "Send the newest diagnostics to Flymake, if they changed or ALWAYS is non-nil."
  (unless flymake-mode
    ;; Flymake would still take the report, and show it.  When Flymake starts again, it calls the
    ;; backend with a new report function.
    (setq typst-canvas--report-fn nil))
  (when (and typst-canvas--report-fn typst-canvas--session)
    (let ((diagnostics (typst-canvas--session-diagnostics typst-canvas--session)))
      (when (or always (not (equal diagnostics typst-canvas--reported)))
        (setq typst-canvas--reported diagnostics)
        (condition-case err
            ;; Flymake adds the diagnostics of later reports in one check to those of the first.
            ;; The whole buffer as the region makes each report replace them.  A new check, which
            ;; would also replace them, starts only after idle time: maybe much later.
            (funcall typst-canvas--report-fn
                     (mapcar #'typst-canvas--make-diagnostic diagnostics)
                     :region (save-restriction
                               (widen)
                               (cons (point-min) (point-max))))
          (error
           ;; E.g. Flymake disabled the backend.  Do not call the function again: Flymake's next
           ;; check gives a new one.
           (setq typst-canvas--report-fn nil)
           (signal (car err) (cdr err))))))))

(defun typst-canvas--make-diagnostic (diagnostic)
  "Make a Flymake diagnostic from DIAGNOSTIC.
DIAGNOSTIC is an element of `typst-canvas--session-diagnostics'."
  (pcase-let* ((`(,beg ,end ,severity ,message) diagnostic)
               ;; The buffer can be shorter than the text sent, if it changed since then.  Not
               ;; `point-max': the diagnostic can be outside the narrowed region.
               (last (1+ (buffer-size)))
               (beg (min beg last))
               (end (min (max end (1+ beg)) last)))
    (flymake-make-diagnostic (current-buffer) beg end severity message)))

;;;; Preview buffer

(defvar-keymap typst-canvas-preview-mode-map
  :doc "Keymap for `typst-canvas-preview-mode'."
  "+" #'typst-canvas-zoom-in
  "=" #'typst-canvas-zoom-in
  "-" #'typst-canvas-zoom-out
  "0" #'typst-canvas-zoom-fit
  "n" #'typst-canvas-next-page
  "p" #'typst-canvas-previous-page
  "t" #'typst-canvas-toggle-theme
  "<left>" #'typst-canvas-scroll-right
  "<right>" #'typst-canvas-scroll-left
  "RET" #'typst-canvas-jump-at-point
  "<mouse-1>" #'typst-canvas-mouse-jump)

(define-derived-mode typst-canvas-preview-mode special-mode "Typst-Canvas"
  "Major mode for the preview of a Typst buffer.
Each page is one canvas image on its own line."
  (setq truncate-lines t)
  (setq cursor-type nil)
  ;; Scrolling to a point in a page leaves point on a line that can be partly visible.
  (setq-local make-cursor-line-fully-visible nil)
  ;; Pages wider than the window (zoom over 100%) scroll horizontally.  Point is always at the
  ;; start of a page line, so automatic hscroll would undo every scroll.  Like `image-mode'.
  (setq-local auto-hscroll-mode nil)
  (setq header-line-format '(:eval (typst-canvas--header-line)))
  (setq-local revert-buffer-function #'typst-canvas--revert)
  (add-hook 'window-size-change-functions #'typst-canvas--on-resize nil t)
  (add-hook 'kill-buffer-hook #'typst-canvas--on-preview-kill nil t))

(defun typst-canvas--window-width ()
  "Return the body width of the window that shows the current buffer, in pixels."
  (let ((window (get-buffer-window (current-buffer) t)))
    (if (and window (display-graphic-p (window-frame window)))
        (window-body-width window t)
      (or typst-canvas--width typst-canvas-default-width))))

(defun typst-canvas--show-pages (session count)
  "Show COUNT pages of SESSION.  Copy only pages whose image changed."
  (typst-canvas--set-page-count count)
  (dotimes (index count)
    (unless (eql (car (typst-canvas--page-info session index))
                 (aref typst-canvas--serials index))
      (typst-canvas--present session index))))

(defun typst-canvas--present (session index)
  "Copy the image of page INDEX of SESSION into its canvas, with the caret."
  (pcase-let ((`(,serial ,width ,height . ,_) (typst-canvas--page-info session index))
              (canvas (aref typst-canvas--canvases index)))
    (when serial
      ;; A canvas spec is a plist after `image'.  `plist-put' changes existing keys in place, so
      ;; the spec stays the same object, and so the same canvas.
      (plist-put (cdr canvas) :data-width width)
      (plist-put (cdr canvas) :data-height height)
      ;; If the thread replaced the image meanwhile, the sizes can differ, and nothing is copied.
      ;; Its notification comes next.
      (when (typst-canvas--present-page session index canvas)
        (aset typst-canvas--serials index serial)))))

(defun typst-canvas--set-page-count (count)
  "Add or remove page lines at the end, so that there are COUNT."
  (let ((old (length typst-canvas--canvases))
        (inhibit-read-only t))
    (cond
     ((> count old)
      (let ((new (cl-loop for index from old below count
                          ;; Canvases are identified by `eq' spec, but the image cache matches
                          ;; specs by `equal'.  An uninterned :id keeps equal-sized pages apart.
                          collect (list 'image :type 'canvas
                                        :id (make-symbol (format "typst-canvas-page-%d" index))
                                        :data-width 1 :data-height 1))))
        (save-excursion
          (goto-char (point-max))
          (cl-loop for canvas in new
                   for index from old
                   do (insert (propertize " " 'display canvas 'typst-canvas-page index 'pointer 'hand)
                              "\n")))
        (setq typst-canvas--canvases (vconcat typst-canvas--canvases new))
        (setq typst-canvas--serials (vconcat typst-canvas--serials (make-vector (- count old) nil)))))
     ((< count old)
      (delete-region (typst-canvas--page-position count) (point-max))
      (setq typst-canvas--canvases (seq-subseq typst-canvas--canvases 0 count))
      (setq typst-canvas--serials (seq-subseq typst-canvas--serials 0 count))))))

(defun typst-canvas--page-position (index)
  "Return the buffer position of page INDEX (0-based)."
  ;; Each page is one image char and a newline.
  (+ (point-min) (* 2 index)))

(defun typst-canvas--source-status ()
  "Return (SENT STATUS) of the source buffer, or nil."
  (when (buffer-live-p typst-canvas--source)
    (with-current-buffer typst-canvas--source
      (list typst-canvas--sent typst-canvas--status))))

(defun typst-canvas--header-line ()
  "Return the header line of the preview: status, page, times, zoom."
  (pcase-let* ((`(,sent ,status) (typst-canvas--source-status))
               (`(,served ,pages ,errors ,warnings ,compile-ms ,render-ms) status)
               (state (cond
                       ((null served) (propertize "stopped" 'face 'shadow))
                       ((< served sent) (propertize "compiling" 'face 'shadow))
                       ((> errors 0)
                        (concat (propertize (typst-canvas--count errors "error") 'face 'error)
                                ;; The pages are from the last good compile.
                                (when (> pages 0)
                                  (concat " " (propertize "stale" 'face 'warning)))))
                       ((> warnings 0)
                        (propertize (typst-canvas--count warnings "warning") 'face 'warning))
                       (t (propertize "ok" 'face 'success)))))
    (concat " "
            (string-join
             (delq nil
                   (list state
                         (when (and served (> pages 0))
                           (format "p %d/%d" (1+ (min (typst-canvas--shown-page) (1- pages)))
                                   pages))
                         (when served
                           (propertize (format "compile %d ms" (round compile-ms)) 'face 'shadow))
                         (when served
                           (propertize (format "render %d ms" (round render-ms)) 'face 'shadow))
                         ;; "%%%%" makes "%%", which the header line shows as "%".
                         (format "%d%%%%" (round (* 100 typst-canvas--zoom)))))
             (propertize " · " 'face 'shadow)))))

(defun typst-canvas--count (count noun)
  "Return COUNT and NOUN, e.g. \"1 error\" or \"2 errors\"."
  (format "%d %s%s" count noun (if (= count 1) "" "s")))

(defun typst-canvas--shown-page ()
  "Return the page of the caret, or else the top page of the selected window."
  (or typst-canvas--caret-page
      (/ (- (window-start) (point-min)) 2)))

(defun typst-canvas--request-view ()
  "Ask the session of this preview to re-render for the current window and zoom."
  (when (buffer-live-p typst-canvas--source)
    (with-current-buffer typst-canvas--source
      (when typst-canvas--session
        (typst-canvas--request nil)))))

(defun typst-canvas--on-resize (window)
  "Re-render if the width of WINDOW changed."
  (typst-canvas--with-guard
    (with-current-buffer (window-buffer window)
      (when (and (display-graphic-p (window-frame window))
                 (not (eql (window-body-width window t) typst-canvas--width)))
        (typst-canvas--request-view)))))

(defun typst-canvas--revert (&rest _)
  "Compile the source buffer again."
  (when (buffer-live-p typst-canvas--source)
    (with-current-buffer typst-canvas--source
      (when typst-canvas--session
        (typst-canvas--send-text)))))

(defun typst-canvas--on-preview-kill ()
  "Turn off `typst-canvas-mode' in the source buffer."
  (typst-canvas--with-guard
    (let ((preview (current-buffer)))
      (when (buffer-live-p typst-canvas--source)
        (with-current-buffer typst-canvas--source
          (when (and typst-canvas-mode (eq typst-canvas--preview preview))
            (setq typst-canvas--preview nil)
            (typst-canvas-mode -1)))))))

(defun typst-canvas--set-zoom (zoom)
  "Set the zoom of the preview to ZOOM, within `typst-canvas-zoom-range'."
  (setq typst-canvas--zoom (max (car typst-canvas-zoom-range)
                                (min (cdr typst-canvas-zoom-range) zoom)))
  ;; Up to 100%, pages are not wider than the window.
  (when (<= typst-canvas--zoom 1)
    (dolist (window (get-buffer-window-list (current-buffer) nil t))
      (set-window-hscroll window 0)))
  (typst-canvas--request-view))

(defconst typst-canvas--hscroll-step 8
  "Columns that `typst-canvas-scroll-left' and `typst-canvas-scroll-right' scroll.")

(defun typst-canvas-scroll-left (&optional count)
  "Scroll the pages left by COUNT steps of `typst-canvas--hscroll-step' columns."
  (interactive "p" typst-canvas-preview-mode)
  (scroll-left (* (or count 1) typst-canvas--hscroll-step)))

(defun typst-canvas-scroll-right (&optional count)
  "Scroll the pages right by COUNT steps of `typst-canvas--hscroll-step' columns."
  (interactive "p" typst-canvas-preview-mode)
  (scroll-right (* (or count 1) typst-canvas--hscroll-step)))

(defun typst-canvas-zoom-in ()
  "Zoom in by `typst-canvas-zoom-step'."
  (interactive nil typst-canvas-preview-mode)
  (typst-canvas--set-zoom (* typst-canvas--zoom typst-canvas-zoom-step)))

(defun typst-canvas-zoom-out ()
  "Zoom out by `typst-canvas-zoom-step'."
  (interactive nil typst-canvas-preview-mode)
  (typst-canvas--set-zoom (/ typst-canvas--zoom typst-canvas-zoom-step)))

(defun typst-canvas-zoom-fit ()
  "Fit the page width to the window."
  (interactive nil typst-canvas-preview-mode)
  (typst-canvas--set-zoom 1.0))

(defun typst-canvas-toggle-theme ()
  "Toggle whether the pages of this preview have the colors of the theme.
See `typst-canvas-match-theme'."
  (interactive nil typst-canvas-preview-mode)
  (setq-local typst-canvas-match-theme (not typst-canvas-match-theme))
  (typst-canvas--request-view)
  (message "Theme colors %s" (if typst-canvas-match-theme "on" "off")))

;;;; Forward sync: from the source cursor to a caret on a page

(defun typst-canvas--on-post-command ()
  "Move the caret, and update the equation at point.
Do it after point stays still for `typst-canvas--follow-delay'."
  (typst-canvas--with-guard
    (unless (and (eql (point) typst-canvas--updated-point)
                 (eql (and typst-canvas-follow-cursor (point)) typst-canvas--caret-point))
      (when typst-canvas--caret-timer
        (cancel-timer typst-canvas--caret-timer))
      (setq typst-canvas--caret-timer
            (run-with-timer typst-canvas--follow-delay nil
                            #'typst-canvas--update-caret-of (current-buffer))))))

(defun typst-canvas--update-caret-of (buffer)
  "Move the caret of BUFFER, if it still has a session."
  (typst-canvas--with-guard
    (when (buffer-live-p buffer)
      (with-current-buffer buffer
        (setq typst-canvas--caret-timer nil)
        (when typst-canvas--session
          (typst-canvas--update-caret))))))

(defun typst-canvas--update-caret ()
  "Show point as the caret in the preview, and scroll to it if it is out of view.
Hide the caret if `typst-canvas-follow-cursor' is nil.  Then update the
equation at point, which shows the caret too.

Do nothing while the buffer has changes that are not sent: the session
maps positions to the newest text sent, so point would be off.  The
result of the send calls this again."
  (unless typst-canvas--text-timer
    (typst-canvas--update-caret-now)))

(defun typst-canvas--update-caret-now ()
  "Do the work of `typst-canvas--update-caret'."
  (let ((cursor (and typst-canvas-follow-cursor (point)))
        (session typst-canvas--session))
    (setq typst-canvas--caret-point cursor
          typst-canvas--updated-point (point))
    (pcase-let ((`(,old ,new ,top ,bottom)
                 (typst-canvas--session-set-caret
                  session (and cursor (1- cursor))
                  (typst-canvas--color-value (face-background 'cursor nil t)))))
      (when (buffer-live-p typst-canvas--preview)
        (with-current-buffer typst-canvas--preview
          (setq typst-canvas--caret-page new)
          ;; Only the pages of the old and the new caret change.  The session answers from the
          ;; output of the last status, which has as many pages as there are canvases, but check
          ;; anyway: a page index past the canvases must not signal.
          (dolist (page (delete-dups (delq nil (list old new))))
            (when (< page (length typst-canvas--canvases))
              (typst-canvas--present session page)))
          (when (and new (< new (length typst-canvas--canvases)))
            (typst-canvas--scroll-to-caret new top bottom)))))
    (typst-canvas--update-equation)))

(defun typst-canvas--scroll-to-caret (page top bottom)
  "Scroll to pixel rows TOP to BOTTOM of PAGE, if they are not visible."
  (when-let* ((window (get-buffer-window (current-buffer) t)))
    (unless (typst-canvas--rows-visible-p window page top bottom)
      (typst-canvas--scroll-to window page top))))

(defun typst-canvas--rows-visible-p (window page top bottom)
  "Return non-nil if pixel rows TOP to BOTTOM of PAGE are visible in WINDOW."
  (pcase (pos-visible-in-window-p (typst-canvas--page-position page) window t)
    ('nil nil)
    (`(,_x ,_y) t)
    (`(,_x ,_y ,hidden-top ,hidden-bottom . ,_)
     (and (>= top hidden-top)
          (<= bottom (- (typst-canvas--page-height page) hidden-bottom))))))

;;;; Equation at point

(defun typst-canvas--update-equation ()
  "Show the equation at point below its last source line, or hide it.
Do nothing while a request is pending: its result calls this again.
Call it only when all buffer changes are sent: see `typst-canvas--update-caret'."
  (cond
   ((not typst-canvas-inline-math)
    (typst-canvas--hide-equation))
   ((and typst-canvas--status (>= (car typst-canvas--status) typst-canvas--sent))
    (pcase (typst-canvas--session-equation typst-canvas--session (1- (point))
                                           (typst-canvas--equation-px-per-em)
                                           (typst-canvas--equation-max-width))
      (`(,end ,_serial ,width ,height ,_stale)
       (typst-canvas--show-equation end width height))
      (_ (typst-canvas--hide-equation))))))

(defun typst-canvas--show-equation (end width height)
  "Show the equation image, WIDTH x HEIGHT, below the line of position END."
  (let ((canvas (or typst-canvas--equation-canvas
                    (setq typst-canvas--equation-canvas
                          (list 'image :type 'canvas
                                :id (make-symbol "typst-canvas-equation")
                                :data-width 1 :data-height 1))))
        ;; The end can be outside the narrowed region.
        (eol (save-excursion
               (save-restriction
                 (widen)
                 (goto-char end)
                 (line-end-position)))))
    (plist-put (cdr canvas) :data-width width)
    (plist-put (cdr canvas) :data-height height)
    (when (typst-canvas--present-equation typst-canvas--session canvas)
      (if typst-canvas--equation-overlay
          (move-overlay typst-canvas--equation-overlay eol eol)
        ;; Front advance: text typed at the end of the line goes before the image.
        (setq typst-canvas--equation-overlay (make-overlay eol eol nil t nil)))
      ;; With point at the end of the line, `cursor' keeps the cursor there, not after the image.
      ;; Setting the string again makes redisplay see a resized canvas.
      (overlay-put typst-canvas--equation-overlay 'after-string
                   (concat (propertize "\n" 'cursor t)
                           (propertize " " 'display canvas))))))

(defun typst-canvas--hide-equation ()
  "Hide the equation at point."
  (when typst-canvas--equation-overlay
    (delete-overlay typst-canvas--equation-overlay)
    (setq typst-canvas--equation-overlay nil)))

(defun typst-canvas--equation-px-per-em ()
  "Return the font size of the equation at point, in pixels, as a float."
  (* 1.0 typst-canvas-inline-math-scale
     (let ((font (and (display-graphic-p) (face-font 'default))))
       (or (and font (aref (font-info font) 2))
           typst-canvas--fallback-text-size))))

(defun typst-canvas--equation-max-width ()
  "Return the width limit of the equation at point, in pixels."
  (let ((window (get-buffer-window (current-buffer) t)))
    (if (and window (display-graphic-p (window-frame window)))
        (max 1 (- (window-body-width window t)
                  (* 2 (frame-char-width (window-frame window)))))
      typst-canvas-default-width)))

;;;; Backward sync: from a page to the source

(defconst typst-canvas--scroll-fraction (/ 1.0 3)
  "Scrolling puts its target this fraction of the window height from the top.")

(defconst typst-canvas--jump-at-point-columns '(0.5 0.4 0.6 0.3 0.7 0.2 0.8)
  "Where `typst-canvas-jump-at-point' tries a page, as fractions of its width.
The middle of a line can be a space between words, which leads nowhere.")

(defun typst-canvas-mouse-jump (event)
  "Jump to the source of what EVENT clicked on a page."
  (interactive "e" typst-canvas-preview-mode)
  (let* ((position (event-start event))
         (buffer (window-buffer (posn-window position)))
         (xy (posn-object-x-y position))
         (page (and (posn-point position)
                    (get-text-property (posn-point position) 'typst-canvas-page buffer))))
    (when (and page xy)
      (with-current-buffer buffer
        (typst-canvas--jump page (car xy) (cdr xy))))))

(defun typst-canvas-jump-at-point ()
  "Jump to the source of the middle of the visible part of the page at point."
  (interactive nil typst-canvas-preview-mode)
  (let ((page (get-text-property (point) 'typst-canvas-page)))
    (unless page
      (user-error "No page at point"))
    (let* ((canvas (aref typst-canvas--canvases page))
           (width (image-property canvas :data-width))
           (height (image-property canvas :data-height))
           ;; (X Y RTOP RBOT ...): RTOP and RBOT are the hidden pixels at the top and bottom.
           (visible (cddr (pos-visible-in-window-p (point) nil t)))
           (y (/ (+ (or (nth 0 visible) 0) (- height (or (nth 1 visible) 0))) 2)))
      (unless (cl-some (lambda (fraction) (typst-canvas--jump page (round (* fraction width)) y t))
                       typst-canvas--jump-at-point-columns)
        (message "Nothing to jump to here")))))

(defun typst-canvas--jump (page x y &optional quiet)
  "Jump to the source of pixel X, Y of PAGE.  Return non-nil if there was one.
If QUIET is nil, say when there was none."
  (let ((session (buffer-local-value 'typst-canvas--session typst-canvas--source)))
    (pcase (and session (typst-canvas--session-jump session page x y))
      (`(source ,position)
       (typst-canvas--show-source position)
       t)
      (`(file ,path ,position)
       (find-file-other-window path)
       (typst-canvas--goto position)
       (typst-canvas--pulse)
       t)
      (`(url ,url)
       (browse-url url)
       t)
      (`(position ,target ,target-y)
       (when-let* ((window (get-buffer-window (current-buffer) t)))
         (typst-canvas--scroll-to window target target-y))
       t)
      (_
       (unless quiet
         (message "Nothing to jump to here"))
       nil))))

(defun typst-canvas--show-source (position)
  "Select a window with the source buffer, go to POSITION, and pulse it."
  (let ((source typst-canvas--source))
    (select-window (or (get-buffer-window source) (display-buffer source)))
    (typst-canvas--goto position)
    (typst-canvas--pulse)))

(defun typst-canvas--goto (position)
  "Go to POSITION.  Widen the buffer if POSITION is outside its narrowed region."
  (let ((position (min position (1+ (buffer-size)))))
    (unless (<= (point-min) position (point-max))
      (widen))
    (goto-char position)))

(defun typst-canvas--pulse ()
  "Briefly highlight the word at point, or the line if there is no word."
  (pcase-let ((`(,beg . ,end) (or (bounds-of-thing-at-point 'word)
                                  (cons (line-beginning-position) (line-end-position)))))
    (pulse-momentary-highlight-region beg end)))

(defun typst-canvas--page-height (index)
  "Return the pixel height of the line of page INDEX."
  (image-property (aref typst-canvas--canvases index) :data-height))

(defun typst-canvas--scroll-to (window page y)
  "Scroll WINDOW so that pixel row Y of PAGE is near its top.
See `typst-canvas--scroll-fraction'."
  (let* ((target (- (+ (cl-loop for index below page sum (typst-canvas--page-height index)) y)
                    (round (* typst-canvas--scroll-fraction (window-body-height window t)))))
         (index 0)
         (top 0))
    ;; Find the page line that contains TARGET, and the pixels of it above TARGET.
    (while (and (< (1+ index) (length typst-canvas--canvases))
                (<= (+ top (typst-canvas--page-height index)) target))
      (cl-incf top (typst-canvas--page-height index))
      (cl-incf index))
    (set-window-start window (typst-canvas--page-position index))
    (set-window-point window (typst-canvas--page-position page))
    (set-window-vscroll window (max 0 (- target top)) t)))

(defun typst-canvas--current-page ()
  "Return the index of the page at point."
  (/ (- (point) (point-min)) 2))

(defun typst-canvas--goto-page (index)
  "Show page INDEX at the top of the selected window."
  (let ((index (max 0 (min index (1- (length typst-canvas--canvases))))))
    (goto-char (typst-canvas--page-position index))
    (set-window-start (selected-window) (point))))

(defun typst-canvas-next-page (&optional count)
  "Show the next page, or the COUNTth next page."
  (interactive "p" typst-canvas-preview-mode)
  (typst-canvas--goto-page (+ (typst-canvas--current-page) (or count 1))))

(defun typst-canvas-previous-page (&optional count)
  "Show the previous page, or the COUNTth previous page."
  (interactive "p" typst-canvas-preview-mode)
  (typst-canvas--goto-page (- (typst-canvas--current-page) (or count 1))))

;;;; Presentation

(defvar-keymap typst-canvas-present-mode-map
  :doc "Keymap for `typst-canvas-present-mode'."
  "SPC" #'typst-canvas-present-next
  "n" #'typst-canvas-present-next
  "<right>" #'typst-canvas-present-next
  "<down>" #'typst-canvas-present-next
  "<next>" #'typst-canvas-present-next
  "DEL" #'typst-canvas-present-previous
  "p" #'typst-canvas-present-previous
  "<left>" #'typst-canvas-present-previous
  "<up>" #'typst-canvas-present-previous
  "<prior>" #'typst-canvas-present-previous
  "<home>" #'typst-canvas-present-first
  "<end>" #'typst-canvas-present-last
  "RET" #'typst-canvas-present-goto
  "q" #'typst-canvas-present-quit)

(dotimes (digit 10)
  (keymap-set typst-canvas-present-mode-map (number-to-string digit)
              #'typst-canvas-present-digit))

(define-derived-mode typst-canvas-present-mode special-mode "Typst-Slides"
  "Major mode that shows one page of a Typst buffer at a time, fit to the window.
Slides update live while the source buffer changes.

\\{typst-canvas-present-mode-map}"
  (setq mode-line-format nil
        header-line-format nil
        cursor-type nil
        truncate-lines t)
  (face-remap-add-relative 'default :background "black")
  (face-remap-add-relative 'fringe :background "black")
  (setq typst-canvas--slide-canvas (list 'image :type 'canvas
                                         :id (make-symbol "typst-canvas-slide")
                                         :data-width 1 :data-height 1))
  (let ((inhibit-read-only t))
    (insert (propertize " " 'display typst-canvas--slide-canvas)))
  (goto-char (point-min))
  (add-hook 'window-size-change-functions #'typst-canvas--on-present-resize nil t)
  ;; Buffer-local, it runs when a window starts or stops showing the buffer.
  (add-hook 'window-buffer-change-functions #'typst-canvas--on-present-window-change nil t)
  (add-hook 'kill-buffer-hook #'typst-canvas--on-present-kill nil t)
  (add-hook 'delete-frame-functions #'typst-canvas--on-delete-frame))

;;;###autoload
(defun typst-canvas-present ()
  "Show the pages of the current Typst buffer as slides, one at a time.
Each page fits the whole window, on black, in a new fullscreen frame
\(see `typst-canvas-present-frame').  It starts at the page that the
preview shows.  Edits in the source buffer show live.  Keys:
\\<typst-canvas-present-mode-map>
\\[typst-canvas-present-next], n, <right>: next slide.
\\[typst-canvas-present-previous], p, <left>: previous slide.
Digits, then \\[typst-canvas-present-goto]: go to that slide.
\\[typst-canvas-present-quit]: quit."
  (interactive)
  (let ((source (if (derived-mode-p 'typst-canvas-preview-mode) typst-canvas--source
                  (current-buffer))))
    (with-current-buffer source
      (unless typst-canvas-mode
        (typst-canvas-mode 1))
      (when (buffer-live-p typst-canvas--presentation)
        (kill-buffer typst-canvas--presentation))
      (let ((page (typst-canvas--preview-page))
            (buffer (get-buffer-create
                     (format "*typst-canvas presentation: %s*" (buffer-name)))))
        (with-current-buffer buffer
          (typst-canvas-present-mode)
          (setq typst-canvas--source source
                typst-canvas--slide page))
        (setq typst-canvas--presentation buffer)
        ;; It selects BUFFER, but the request needs the session of SOURCE.
        (save-current-buffer
          (typst-canvas--show-presentation buffer))
        (typst-canvas--request nil)
        (message "Slide %d · SPC next · DEL previous · q quit" (1+ page))))))

(defun typst-canvas--preview-page ()
  "Return the page of the caret, else the top page of the preview window, else 0."
  (or (and (buffer-live-p typst-canvas--preview)
           (with-current-buffer typst-canvas--preview
             (or typst-canvas--caret-page
                 (when-let* ((window (get-buffer-window (current-buffer) t)))
                   (/ (- (window-start window) (point-min)) 2)))))
      0))

(defun typst-canvas--show-presentation (buffer)
  "Show the presentation BUFFER alone in a window: in a new frame, or the selected one."
  (if typst-canvas-present-frame
      (let ((frame (make-frame typst-canvas--present-frame-parameters)))
        ;; Also the fringes of the minibuffer window.
        (set-face-background 'fringe "black" frame)
        (with-current-buffer buffer
          (setq typst-canvas--present-frame frame))
        (select-frame-set-input-focus frame))
    (let ((windows (current-window-configuration)))
      (with-current-buffer buffer
        (setq typst-canvas--present-windows windows))))
  (switch-to-buffer buffer)
  (delete-other-windows)
  (let ((window (selected-window)))
    ;; See `typst-canvas--present-frame-parameters' for the fringes.
    (set-window-fringes window 8 8)
    (set-window-margins window 0 0)
    (set-window-scroll-bars window 0 nil 0 nil)))

(defun typst-canvas--slide-view ()
  "Return [PAGE WIDTH HEIGHT] of the slide that the current presentation buffer shows.
Return nil if no window shows the buffer: then no slide is rendered."
  (if-let* ((window (get-buffer-window (current-buffer) t)))
      (let ((size (if (display-graphic-p (window-frame window))
                      (cons (window-body-width window t) (window-body-height window t))
                    (cons typst-canvas-default-width
                          (round (* typst-canvas--present-aspect typst-canvas-default-width))))))
        (setq typst-canvas--slide-size size)
        (vector typst-canvas--slide (car size) (cdr size)))
    (setq typst-canvas--slide-size nil)))

(defun typst-canvas--show-slide ()
  "Copy the newest slide of the session of the current buffer into its presentation."
  (let ((session typst-canvas--session)
        (count (nth 1 typst-canvas--status)))
    (with-current-buffer typst-canvas--presentation
      ;; The document can lose pages.  The thread then shows the last one.
      (when (and (> count 0) (>= typst-canvas--slide count))
        (setq typst-canvas--slide (1- count)))
      (pcase (typst-canvas--slide-info session)
        ((and `(,serial ,width ,height) (guard (not (eql serial typst-canvas--slide-serial))))
         (plist-put (cdr typst-canvas--slide-canvas) :data-width width)
         (plist-put (cdr typst-canvas--slide-canvas) :data-height height)
         (when (typst-canvas--present-slide session typst-canvas--slide-canvas)
           (setq typst-canvas--slide-serial serial)))))))

(defun typst-canvas--slide-count ()
  "Return the number of pages of the source of the current presentation buffer."
  (or (nth 1 (cadr (typst-canvas--source-status))) 0))

(defun typst-canvas--present-show (page)
  "Show slide PAGE (0-based), within the pages of the document."
  (let ((count (typst-canvas--slide-count)))
    (setq typst-canvas--slide (max 0 (min page (1- count)))
          typst-canvas--slide-digits "")
    (typst-canvas--request-view)
    (message "Slide %d/%d" (1+ typst-canvas--slide) count)))

(defun typst-canvas-present-next (&optional count)
  "Show the next slide, or the COUNTth next one."
  (interactive "p" typst-canvas-present-mode)
  (typst-canvas--present-show (+ typst-canvas--slide (or count 1))))

(defun typst-canvas-present-previous (&optional count)
  "Show the previous slide, or the COUNTth previous one.
If digits were typed for `typst-canvas-present-goto', delete the last one."
  (interactive "p" typst-canvas-present-mode)
  (if (string-empty-p typst-canvas--slide-digits)
      (typst-canvas--present-show (- typst-canvas--slide (or count 1)))
    (setq typst-canvas--slide-digits (substring typst-canvas--slide-digits 0 -1))
    (message "Go to slide: %s" typst-canvas--slide-digits)))

(defun typst-canvas-present-first ()
  "Show the first slide."
  (interactive nil typst-canvas-present-mode)
  (typst-canvas--present-show 0))

(defun typst-canvas-present-last ()
  "Show the last slide."
  (interactive nil typst-canvas-present-mode)
  (typst-canvas--present-show (1- (typst-canvas--slide-count))))

(defun typst-canvas-present-digit ()
  "Add the typed digit to the slide number for `typst-canvas-present-goto'."
  (interactive nil typst-canvas-present-mode)
  (setq typst-canvas--slide-digits
        (concat typst-canvas--slide-digits (string last-command-event)))
  (message "Go to slide: %s" typst-canvas--slide-digits))

(defun typst-canvas-present-goto ()
  "Show the slide whose number (1-based) was typed as digits."
  (interactive nil typst-canvas-present-mode)
  (if (string-empty-p typst-canvas--slide-digits)
      (message "Type a slide number, then RET")
    (typst-canvas--present-show (1- (string-to-number typst-canvas--slide-digits)))))

(defun typst-canvas-present-quit ()
  "End the presentation."
  (interactive nil typst-canvas-present-mode)
  (kill-buffer (current-buffer)))

(defun typst-canvas--on-present-resize (window)
  "Ask for a new slide if the size of WINDOW changed."
  (typst-canvas--with-guard
    (with-current-buffer (window-buffer window)
      (when (and (display-graphic-p (window-frame window))
                 (not (equal (cons (window-body-width window t) (window-body-height window t))
                             typst-canvas--slide-size)))
        (typst-canvas--request-view)))))

(defun typst-canvas--on-present-window-change (_window)
  "Ask for slides while a window shows the presentation, and stop when none does."
  (typst-canvas--with-guard
    (typst-canvas--request-view)))

(defun typst-canvas--presentation-buffers ()
  "Return the live presentation buffers."
  (seq-filter (lambda (buffer)
                (eq (buffer-local-value 'major-mode buffer) 'typst-canvas-present-mode))
              (buffer-list)))

(defun typst-canvas--on-delete-frame (frame)
  "End the presentations that FRAME shows, e.g. when the window manager closes it.
They are in a frame made for them, or in a window of FRAME."
  (typst-canvas--with-guard
    (dolist (buffer (typst-canvas--presentation-buffers))
      (with-current-buffer buffer
        (when (or (eq typst-canvas--present-frame frame)
                  (and typst-canvas--present-windows
                       (eq (window-configuration-frame typst-canvas--present-windows) frame)))
          ;; FRAME goes away: do not delete it again, nor restore windows into it.
          (setq typst-canvas--present-frame nil
                typst-canvas--present-windows nil)
          (kill-buffer buffer))))))

(defun typst-canvas--on-present-kill ()
  "Stop rendering slides, and close the presentation frame or restore the windows."
  (typst-canvas--with-guard
    (let ((presentation (current-buffer))
          (frame typst-canvas--present-frame)
          (windows typst-canvas--present-windows))
      (when (buffer-live-p typst-canvas--source)
        (with-current-buffer typst-canvas--source
          (when (eq typst-canvas--presentation presentation)
            (setq typst-canvas--presentation nil)
            (when typst-canvas--session
              (typst-canvas--request nil)))))
      (unless (cdr (typst-canvas--presentation-buffers))
        (remove-hook 'delete-frame-functions #'typst-canvas--on-delete-frame))
      (cond
       ((and (frame-live-p frame) (cdr (frame-list)))
        (delete-frame frame))
       (windows
        (set-window-configuration windows))))))

;;;; Demo

(autoload 'typst-canvas-demo "typst-canvas-demo"
  "Show `typst-canvas-mode' on a showcase document that types itself." t)

(provide 'typst-canvas)
;;; typst-canvas.el ends here
