;;; screenshot.el --- Screenshots of typst-canvas, for bin/screenshot.sh -*- lexical-binding: t -*-

;; Opens examples/sample.typ with `typst-canvas-mode', then saves target/screenshot-N.png at each
;; step: fit width, a compile error, zoomed in on page 2.

(require 'cl-lib)
(require 'typst-canvas)

(defconst typst-canvas-screenshot--root
  (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))

(defconst typst-canvas-screenshot--timeout 60
  "Seconds after which the run fails.  The first compile scans the system fonts.")

(defconst typst-canvas-screenshot--redisplay-delay 0.5
  "Seconds between a step and its screenshot, for redisplay.")

(defvar typst-canvas-screenshot--source nil)

(defun typst-canvas-screenshot--settled-p ()
  "Return non-nil if the newest request is served, and all pages are shown."
  (with-current-buffer typst-canvas-screenshot--source
    (and typst-canvas--status
         (>= (car typst-canvas--status) typst-canvas--sent)
         (with-current-buffer typst-canvas--preview
           (and (> (length typst-canvas--serials) 0)
                (cl-every #'identity typst-canvas--serials))))))

(defun typst-canvas-screenshot--save (name)
  "Save a PNG of the frame as target/NAME."
  (let ((png (x-export-frames nil 'png))
        (file (expand-file-name (concat "target/" name) typst-canvas-screenshot--root)))
    (with-temp-file file
      (set-buffer-multibyte nil)
      (insert png))
    (message "Saved %s" file)))

(defun typst-canvas-screenshot--run (steps)
  "Run STEPS, a list of functions.  Before each, wait until the preview settles."
  (cond
   ((null steps) (kill-emacs 0))
   ((typst-canvas-screenshot--settled-p)
    (funcall (car steps))
    (run-with-timer typst-canvas-screenshot--redisplay-delay nil
                    #'typst-canvas-screenshot--run (cdr steps)))
   (t (run-with-timer 0.1 nil #'typst-canvas-screenshot--run steps))))

(defun typst-canvas-screenshot--in-preview (function)
  "Call FUNCTION in the window of the preview."
  (with-selected-window (get-buffer-window
                         (buffer-local-value 'typst-canvas--preview typst-canvas-screenshot--source))
    (funcall function)))

(set-frame-size nil 1280 800 t)
(delete-other-windows)
(find-file (expand-file-name "examples/sample.typ" typst-canvas-screenshot--root))
(setq typst-canvas-screenshot--source (current-buffer))
;; The steps edit the file, but must not leave auto-save or lock files.
(auto-save-mode -1)
(setq-local create-lockfiles nil)
(typst-canvas-mode 1)
(run-with-timer typst-canvas-screenshot--timeout nil #'kill-emacs 1)
(run-with-timer
 0.5 nil #'typst-canvas-screenshot--run
 (list
  ;; Wait for redisplay after the first result, so that the window width is final.
  #'ignore
  (lambda () (typst-canvas-screenshot--save "screenshot-1.png"))
  (lambda ()
    (with-current-buffer typst-canvas-screenshot--source
      (goto-char (point-max))
      (insert "\n#nope")
      (flymake-start)))
  (lambda () (typst-canvas-screenshot--save "screenshot-2.png"))
  (lambda ()
    (with-current-buffer typst-canvas-screenshot--source
      (delete-region (- (point-max) 6) (point-max)))
    (typst-canvas-screenshot--in-preview
     (lambda ()
       (typst-canvas-zoom-in)
       (typst-canvas-zoom-in)
       (typst-canvas-next-page))))
  (lambda () (typst-canvas-screenshot--save "screenshot-3.png"))))

;;; screenshot.el ends here
