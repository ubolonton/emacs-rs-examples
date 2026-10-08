;;; typst-canvas-demo.el --- Self-typing demo of typst-canvas -*- lexical-binding: t -*-

;;; Commentary:

;; `typst-canvas-demo' opens examples/showcase.typ in a buffer that does not visit the file, turns
;; on `typst-canvas-mode', and "ghost types" into it: a sentence, an equation, a table row.  Then it
;; switches the theme and back, and zooms in and out.  Each step runs from a timer, so Emacs stays
;; responsive.  Any command stops the demo, and keeps the buffer for the user.

;;; Code:

(require 'cl-lib)
(require 'color)
(require 'typst-canvas)

(defcustom typst-canvas-demo-speed 1.0
  "Speed of `typst-canvas-demo'.  2 runs twice as fast."
  :type 'number
  :group 'typst-canvas)

(defconst typst-canvas-demo--root
  (file-name-directory (or load-file-name buffer-file-name))
  "Directory of typst-canvas, which has examples/showcase.typ.")

(defconst typst-canvas-demo--buffer-name "showcase.typ (demo)")

(defconst typst-canvas-demo--key-delay 0.09
  "Seconds between two typed chars, before jitter and pauses at word ends.")

(defconst typst-canvas-demo--jitter 0.45
  "Largest random change of a key delay, as a fraction of it.")

(defconst typst-canvas-demo--snippet-delay 0.5
  "Seconds after a snippet expansion: the time to see what it inserted.")

(defconst typst-canvas-demo--poll-delay 0.1
  "Seconds between two checks whether the first pages are shown.")

(defconst typst-canvas-demo--pairs '((?\( . ?\)) (?\[ . ?\]) (?* . ?*))
  "Openers that the demo types with their closer, like `electric-pair-mode'.
The text stays balanced while it grows, so most of the intermediate
states compile, and the preview updates on most keys.")

(defconst typst-canvas-demo--script
  '((wait-for-preview)
    (pause 1.5)
    (goto "[typst.app/docs].")
    (type " This sentence is typed live, and the page reflows as it grows.")
    (pause 1.2)
    (goto "dif x $\n")
    (type "\nThe Basel problem, solved by Euler in 1734:\n")
    ;; Display math needs the spaces inside the dollars from the start, else it is inline.
    (snippet "$ " " $\n")
    (type "sum_(n=1)^oo 1/n^2 = pi^2/6")
    (pause 1.2)
    ;; After the last argument, a row stays valid while it grows: no comma is missing.
    (goto "table.hline(),")
    (type "\n  [*Total*], [From key press to screen], [*25 ms*],")
    (pause 1.5)
    (switch-theme)
    (pause 3.0)
    (restore-theme)
    (pause 2.0)
    (zoom typst-canvas-zoom-in)
    (pause 0.6)
    (zoom typst-canvas-zoom-in)
    (pause 2.0)
    (zoom typst-canvas-zoom-out)
    (pause 0.6)
    (zoom typst-canvas-zoom-out)
    (pause 0.6)
    (zoom typst-canvas-zoom-out)
    (pause 0.6)
    (zoom typst-canvas-zoom-out)
    (pause 2.0)
    (zoom typst-canvas-zoom-fit))
  "Steps of the demo.  See `typst-canvas-demo--run'.")

(defconst typst-canvas-demo--keywords
  '(("\"\\(?:[^\"\\\n]\\|\\\\.\\)*\"" . font-lock-string-face)
    ("\\(?:^\\|[[:space:]]\\)\\(//.*\\)" 1 font-lock-comment-face)
    ("^=+ .*" . font-lock-function-name-face)
    ("#[[:alpha:]][[:alnum:]_.-]*" . font-lock-keyword-face)
    ("\\$[^$\n]*\\$" . font-lock-constant-face))
  "Minimal Typst highlighting, if there is no `typst-ts-mode'.")

(defvar typst-canvas-demo--buffer nil "Source buffer of the running demo, or nil.")
(defvar typst-canvas-demo--timer nil "Timer of the next step.")
(defvar typst-canvas-demo--steps nil "Steps that are not done yet.")
(defvar typst-canvas-demo--themes nil
  "(THEMES), with the themes that were on before the demo switched them, or nil.")

;;;###autoload
(defun typst-canvas-demo ()
  "Show `typst-canvas-mode' on a showcase document that types itself.
Open examples/showcase.typ in a buffer that does not visit the file,
and type a sentence, an equation and a table row at human speed, so the
preview updates live.  Then switch between `modus-operandi' and
`modus-vivendi' and back, and zoom in and out.  Any command stops the
demo.  The buffer stays, for more edits.  See `typst-canvas-demo-speed'."
  (interactive)
  (typst-canvas-demo-stop)
  (when-let* ((old (get-buffer typst-canvas-demo--buffer-name)))
    (kill-buffer old))
  (let ((buffer (get-buffer-create typst-canvas-demo--buffer-name)))
    (with-current-buffer buffer
      (insert-file-contents (expand-file-name "examples/showcase.typ" typst-canvas-demo--root))
      ;; Relative paths in the document resolve from here.
      (setq default-directory (expand-file-name "examples/" typst-canvas-demo--root))
      (if (fboundp 'typst-ts-mode)
          (typst-ts-mode)
        (setq-local font-lock-defaults '(typst-canvas-demo--keywords t))
        (font-lock-mode 1))
      ;; Undo must not remove the whole document.
      (setq buffer-undo-list nil)
      (set-buffer-modified-p nil)
      (add-hook 'kill-buffer-hook #'typst-canvas-demo-stop nil t))
    (switch-to-buffer buffer)
    (delete-other-windows)
    (with-current-buffer buffer
      (typst-canvas-mode 1))
    (setq typst-canvas-demo--buffer buffer
          typst-canvas-demo--steps typst-canvas-demo--script)
    (add-hook 'pre-command-hook #'typst-canvas-demo-stop)
    (typst-canvas-demo--schedule 0)
    (message "typst-canvas demo: press any key to stop")))

(defun typst-canvas-demo-stop ()
  "Stop the running demo, and restore the theme.  Keep its buffer."
  (interactive)
  (when typst-canvas-demo--buffer
    (typst-canvas-demo--end "stopped")))

(defun typst-canvas-demo--end (how)
  "End the demo, and say HOW it ended."
  (when typst-canvas-demo--timer
    (cancel-timer typst-canvas-demo--timer))
  (remove-hook 'pre-command-hook #'typst-canvas-demo-stop)
  (setq typst-canvas-demo--timer nil
        typst-canvas-demo--steps nil
        typst-canvas-demo--buffer nil)
  (typst-canvas-demo--restore-theme)
  (message "typst-canvas demo %s" how))

(defun typst-canvas-demo--schedule (delay)
  "Run the next step after DELAY seconds, scaled by `typst-canvas-demo-speed'."
  (setq typst-canvas-demo--timer
        (run-with-timer (/ delay typst-canvas-demo-speed) nil #'typst-canvas-demo--next)))

(defun typst-canvas-demo--next ()
  "Run the next step, and schedule the one after it."
  (setq typst-canvas-demo--timer nil)
  (cond
   ((not (buffer-live-p typst-canvas-demo--buffer))
    (typst-canvas-demo--end "stopped"))
   ((null typst-canvas-demo--steps)
    (typst-canvas-demo--end "finished"))
   (t
    (condition-case err
        (typst-canvas-demo--schedule
         (with-current-buffer typst-canvas-demo--buffer
           (typst-canvas-demo--run (pop typst-canvas-demo--steps))))
      (error (typst-canvas-demo--end (format "failed: %s" (error-message-string err))))))))

(defun typst-canvas-demo--run (step)
  "Run STEP in the demo buffer.  Return the seconds until the next step.
STEP is one of:
- (wait-for-preview): wait until the preview shows all pages.
- (pause SECONDS)
- (goto TEXT): move point to the end of the first TEXT.
- (type TEXT): type TEXT, one char per step.
- (snippet BEFORE AFTER): insert BEFORE and AFTER at once, with point
  between them, like a snippet expansion.
- (switch-theme): switch between a light and a dark Modus theme.
- (restore-theme): turn the themes from before the switch back on.
- (zoom COMMAND): call COMMAND in the preview, e.g. `typst-canvas-zoom-in'."
  (pcase step
    ('(wait-for-preview)
     (unless (typst-canvas-demo--preview-ready-p)
       (push step typst-canvas-demo--steps))
     typst-canvas-demo--poll-delay)
    (`(pause ,seconds) seconds)
    (`(goto ,text)
     (goto-char (point-min))
     (search-forward text)
     (typst-canvas--on-post-command)
     0)
    (`(type ,text)
     (setq typst-canvas-demo--steps
           (append (mapcar (lambda (char) (list 'key char)) text) typst-canvas-demo--steps))
     0)
    (`(snippet ,before ,after)
     (insert before)
     (save-excursion (insert after))
     (typst-canvas--on-post-command)
     typst-canvas-demo--snippet-delay)
    (`(key ,char)
     (typst-canvas-demo--type char)
     ;; Commands run `post-command-hook', which moves the caret.  Timers do not.
     (typst-canvas--on-post-command)
     (typst-canvas-demo--key-delay char))
    ('(switch-theme) (typst-canvas-demo--switch-theme) 0)
    ('(restore-theme) (typst-canvas-demo--restore-theme) 0)
    (`(zoom ,command)
     (with-current-buffer typst-canvas--preview
       (funcall command))
     0)
    (_ (error "Unknown demo step: %S" step))))

(defun typst-canvas-demo--preview-ready-p ()
  "Return non-nil if the preview shows the newest result, with all pages."
  (and typst-canvas--status
       (>= (car typst-canvas--status) typst-canvas--sent)
       (buffer-live-p typst-canvas--preview)
       (with-current-buffer typst-canvas--preview
         (and (> (length typst-canvas--serials) 0)
              (cl-every #'identity typst-canvas--serials)))))

(defun typst-canvas-demo--type (char)
  "Type CHAR at point.  Type an opener with its closer, and type over a closer.
See `typst-canvas-demo--pairs'."
  (let ((closer (alist-get char typst-canvas-demo--pairs)))
    (cond
     ((and (eql (char-after) char) (rassq char typst-canvas-demo--pairs))
      (forward-char))
     (closer
      (insert char closer)
      (backward-char))
     (t (insert char)))))

(defun typst-canvas-demo--key-delay (char)
  "Return the seconds after typing CHAR: longer after words and sentences, with jitter."
  (* typst-canvas-demo--key-delay
     (pcase char
       (?\n 4)
       ((or ?. ?, ?:) 3)
       (?\s 1.5)
       (_ 1))
     (+ 1 (* typst-canvas-demo--jitter (- (cl-random 2.0) 1)))))

(defun typst-canvas-demo--dark-p ()
  "Return non-nil if the `default' face has a dark background."
  (let ((background (face-background 'default nil t)))
    (if (color-defined-p background)
        (color-dark-p (color-name-to-rgb background))
      (eq (frame-parameter nil 'background-mode) 'dark))))

(defun typst-canvas-demo--switch-theme ()
  "Turn on `modus-operandi' if the background is dark, else `modus-vivendi'.
Remember the themes that were on, for `typst-canvas-demo--restore-theme'."
  (let ((theme (if (typst-canvas-demo--dark-p) 'modus-operandi 'modus-vivendi)))
    (setq typst-canvas-demo--themes (list custom-enabled-themes))
    (mapc #'disable-theme custom-enabled-themes)
    (load-theme theme t)))

(defun typst-canvas-demo--restore-theme ()
  "Turn on the themes from before `typst-canvas-demo--switch-theme' again."
  (when typst-canvas-demo--themes
    (let ((themes (car typst-canvas-demo--themes)))
      (setq typst-canvas-demo--themes nil)
      (mapc #'disable-theme custom-enabled-themes)
      ;; The first theme has the highest precedence, so enable it last.
      (mapc #'enable-theme (reverse themes)))))

(provide 'typst-canvas-demo)
;;; typst-canvas-demo.el ends here
