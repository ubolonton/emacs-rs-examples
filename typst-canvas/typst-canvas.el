;;; typst-canvas.el --- Live Typst preview in canvas images -*- lexical-binding: t -*-

;;; Commentary:

;; Requires Emacs 32 with a window system, and the module `typst-canvas-dyn'.
;;
;; `typst-canvas-mode' in a Typst buffer shows a live preview in another window.  A Rust thread
;; compiles the buffer text and renders the pages.  When a result is ready, the thread writes to a
;; pipe process.  The process filter copies the changed pages into canvas images, one per page.
;; Diagnostics go to Flymake.

;;; Code:

(require 'cl-lib)
(require 'color)
(require 'flymake)
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

(defcustom typst-canvas-match-theme t
  "If non-nil, pages have the colors of the `default' face.
Documents that set their own page or text colors keep them.  Toggle it
in a preview with `typst-canvas-toggle-theme'."
  :type 'boolean)

(defconst typst-canvas--fallback-desk "#808080"
  "Desk color if the `default' face has no known background, e.g. in batch mode.")

(defconst typst-canvas--desk-lightness-shift 8
  "Percent by which the desk lightness differs from the `default' background.
Without it, pages that match the theme would not stand out from the desk.")

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

;;;; State of the preview buffer

(defvar-local typst-canvas--source nil "Source buffer of this preview.")
(defvar-local typst-canvas--canvases [] "Canvas image specs, one per page.")
(defvar-local typst-canvas--serials []
  "Serial of the image last copied into each canvas, or nil.")
(defvar-local typst-canvas--zoom 1.0 "Zoom factor.  1 fits the page width to the window.")
(defvar-local typst-canvas--width nil "Window width of the newest request, in pixels.")

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
                                       (typst-canvas--on-notify source))))
    (setq typst-canvas--session (typst-canvas--session-start root main typst-canvas--process))
    (add-hook 'after-change-functions #'typst-canvas--on-change nil t)
    (add-hook 'kill-buffer-hook #'typst-canvas--stop nil t)
    (add-hook 'flymake-diagnostic-functions #'typst-canvas-flymake nil t)
    (when (and typst-canvas-enable-flymake (not flymake-mode))
      (setq typst-canvas--started-flymake t)
      (flymake-mode 1))
    (typst-canvas--update-theme-hooks)
    (display-buffer preview typst-canvas-display-action)
    (typst-canvas--send-text)))

(defun typst-canvas--stop ()
  "Stop the session of the current buffer, and kill its preview."
  (remove-hook 'after-change-functions #'typst-canvas--on-change t)
  (remove-hook 'kill-buffer-hook #'typst-canvas--stop t)
  (remove-hook 'flymake-diagnostic-functions #'typst-canvas-flymake t)
  (when typst-canvas--text-timer
    (cancel-timer typst-canvas--text-timer)
    (setq typst-canvas--text-timer nil))
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

(defun typst-canvas--on-change (&rest _)
  "Send the buffer text after the current command or timer.
One send covers all the changes of a command, e.g. of `replace-regexp'."
  (unless typst-canvas--text-timer
    (setq typst-canvas--text-timer
          (run-with-timer 0 nil #'typst-canvas--send-text-of (current-buffer)))))

(defun typst-canvas--send-text-of (buffer)
  "Send the text of BUFFER, if it still has a session."
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (setq typst-canvas--text-timer nil)
      (when typst-canvas--session
        (typst-canvas--send-text)))))

(defun typst-canvas--send-text ()
  "Send the whole text of the current buffer to its session."
  (typst-canvas--request (save-restriction
                           (widen)
                           (buffer-substring-no-properties (point-min) (point-max)))))

(defun typst-canvas--request (text)
  "Send TEXT and the preview view to the session of the current buffer.
TEXT nil only re-renders the last good document, or compiles it again
if the theme colors changed."
  (let ((width typst-canvas-default-width)
        (zoom 1.0)
        (match-theme typst-canvas-match-theme))
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
                                           desk page ink)))
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
  (dolist (buffer (buffer-list))
    (with-current-buffer buffer
      (when typst-canvas--session
        (typst-canvas--request nil)))))

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
  (when (and typst-canvas--report-fn typst-canvas--session)
    (let ((diagnostics (typst-canvas--session-diagnostics typst-canvas--session)))
      (when (or always (not (equal diagnostics typst-canvas--reported)))
        (setq typst-canvas--reported diagnostics)
        (funcall typst-canvas--report-fn
                 (mapcar #'typst-canvas--make-diagnostic diagnostics))))))

(defun typst-canvas--make-diagnostic (diagnostic)
  "Make a Flymake diagnostic from DIAGNOSTIC.
DIAGNOSTIC is an element of `typst-canvas--session-diagnostics'."
  (pcase-let* ((`(,beg ,end ,severity ,message) diagnostic)
               ;; The buffer can be shorter than the compiled text, if it changed since then.
               (beg (min beg (point-max)))
               (end (min (max end (1+ beg)) (point-max))))
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
  "t" #'typst-canvas-toggle-theme)

(define-derived-mode typst-canvas-preview-mode special-mode "Typst-Canvas"
  "Major mode for the preview of a Typst buffer.
Each page is one canvas image on its own line."
  (setq truncate-lines t)
  (setq cursor-type nil)
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
    (pcase-let ((`(,serial ,width ,height) (typst-canvas--page-info session index))
                (canvas (aref typst-canvas--canvases index)))
      (unless (or (null serial) (eql serial (aref typst-canvas--serials index)))
        ;; A canvas spec is a plist after `image'.  `plist-put' changes existing keys in place,
        ;; so the spec stays the same object, and so the same canvas.
        (plist-put (cdr canvas) :data-width width)
        (plist-put (cdr canvas) :data-height height)
        ;; If the thread replaced the image meanwhile, the sizes can differ, and nothing is
        ;; copied.  Its notification comes next.
        (when (typst-canvas--present-page session index canvas)
          (aset typst-canvas--serials index serial))))))

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
                   do (insert (propertize " " 'display canvas 'typst-canvas-page index) "\n")))
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
  "Return the header line of the preview: status, pages, times, zoom."
  (pcase-let* ((`(,sent ,status) (typst-canvas--source-status))
               (`(,served ,pages ,errors ,warnings ,compile-ms ,render-ms) status)
               (state (cond
                       ((null served) "stopped")
                       ((< served sent) "compiling")
                       ((> errors 0)
                        (propertize (format "%d error%s" errors (if (= errors 1) "" "s"))
                                    'face 'error))
                       ((> warnings 0)
                        (propertize (format "%d warning%s" warnings (if (= warnings 1) "" "s"))
                                    'face 'warning))
                       (t "ok"))))
    (concat " " state
            (when served
              (format "  %d page%s  compile %d ms  render %d ms"
                      pages (if (= pages 1) "" "s") (round compile-ms) (round render-ms)))
            ;; "%%%%" makes "%%", which the header line shows as "%".
            (format "  %d%%%%" (round (* 100 typst-canvas--zoom))))))

(defun typst-canvas--request-view ()
  "Ask the session of this preview to re-render for the current window and zoom."
  (when (buffer-live-p typst-canvas--source)
    (with-current-buffer typst-canvas--source
      (when typst-canvas--session
        (typst-canvas--request nil)))))

(defun typst-canvas--on-resize (window)
  "Re-render if the width of WINDOW changed."
  (with-current-buffer (window-buffer window)
    (when (and (display-graphic-p (window-frame window))
               (not (eql (window-body-width window t) typst-canvas--width)))
      (typst-canvas--request-view))))

(defun typst-canvas--revert (&rest _)
  "Compile the source buffer again."
  (when (buffer-live-p typst-canvas--source)
    (with-current-buffer typst-canvas--source
      (when typst-canvas--session
        (typst-canvas--send-text)))))

(defun typst-canvas--on-preview-kill ()
  "Turn off `typst-canvas-mode' in the source buffer."
  (let ((preview (current-buffer)))
    (when (buffer-live-p typst-canvas--source)
      (with-current-buffer typst-canvas--source
        (when (and typst-canvas-mode (eq typst-canvas--preview preview))
          (setq typst-canvas--preview nil)
          (typst-canvas-mode -1))))))

(defun typst-canvas--set-zoom (zoom)
  "Set the zoom of the preview to ZOOM, within `typst-canvas-zoom-range'."
  (setq typst-canvas--zoom (max (car typst-canvas-zoom-range)
                                (min (cdr typst-canvas-zoom-range) zoom)))
  (typst-canvas--request-view))

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

(provide 'typst-canvas)
;;; typst-canvas.el ends here
