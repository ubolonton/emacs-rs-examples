;;; screenshot.el --- Screenshots of typst-canvas, for bin/screenshot.sh -*- lexical-binding: t -*-

;; Opens examples/sample.typ with `typst-canvas-mode', then saves target/screenshot-N.png at each
;; step:
;; 1. Light theme, with the caret on page 2.
;; 2. A compile error: Flymake, and the stale pages.
;; 3. `modus-vivendi' (dark), loaded at run time, with theme matching.
;; 4. Right after a click on the caret's line: the jump target pulses in the source.
;; 5. Zoomed in, scrolled right.
;; 6. Point in an equation: it shows rendered below its source line.
;; 7. An error in that equation: the equation shows dimmed.

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

(defun typst-canvas-screenshot--move-caret (text)
  "Move point in the source to the end of the first TEXT, and move the caret there."
  (with-current-buffer typst-canvas-screenshot--source
    (goto-char (point-min))
    (search-forward text)
    (typst-canvas--update-caret)))

(defun typst-canvas-screenshot--caret-line-click ()
  "Return a click position on text in the caret's line, in the preview window."
  (with-current-buffer typst-canvas-screenshot--source
    (let ((session typst-canvas--session)
          (preview typst-canvas--preview))
      (pcase-let ((`(,_old ,page ,top ,bottom)
                   (typst-canvas--session-set-caret session (1- (point)) 0)))
        (with-current-buffer preview
          (let* ((window (get-buffer-window preview))
                 (row (/ (+ top bottom) 2))
                 ;; Window and image coordinates differ by a constant: correct a first guess.
                 (guess (cdr (posn-object-x-y (posn-at-x-y 40 100 window))))
                 (window-y (+ 100 (- row guess))))
            (cl-loop for window-x from 40 below (window-body-width window t) by 10
                     for position = (posn-at-x-y window-x window-y window)
                     for xy = (posn-object-x-y position)
                     when (and xy (typst-canvas--session-jump session page (car xy) (cdr xy)))
                     return position)))))))

(set-frame-size nil 1280 800 t)
(delete-other-windows)
(find-file (expand-file-name "examples/sample.typ" typst-canvas-screenshot--root))
(setq typst-canvas-screenshot--source (current-buffer))
;; The steps edit the file, but must not leave auto-save or lock files.
(auto-save-mode -1)
(setq-local create-lockfiles nil)
(typst-canvas-mode 1)
(run-with-timer typst-canvas-screenshot--timeout nil #'kill-emacs 1)
(let (click)
  (run-with-timer
   0.5 nil #'typst-canvas-screenshot--run
   (list
    ;; Wait for redisplay after the first result, so that the window width is final.
    #'ignore
    (lambda () (typst-canvas-screenshot--move-caret "copy changed pa"))
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
      (load-theme 'modus-vivendi t))
    (lambda () (typst-canvas-screenshot--move-caret "[3], [noti"))
    (lambda () (typst-canvas-screenshot--save "screenshot-3.png"))
    (lambda ()
      (setq click (typst-canvas-screenshot--caret-line-click))
      ;; Without the caret, the pulse is the only mark of the jump.
      (with-current-buffer typst-canvas-screenshot--source
        (goto-char (point-min))
        (typst-canvas--update-caret)))
    (lambda ()
      (typst-canvas-mouse-jump (list 'mouse-1 click))
      ;; The pulse fades out, so take the screenshot right away.
      (redisplay t)
      (typst-canvas-screenshot--save "screenshot-4.png"))
    (lambda ()
      (typst-canvas-screenshot--in-preview
       (lambda ()
         (typst-canvas-zoom-in)
         (typst-canvas-zoom-in)
         (typst-canvas-zoom-in)
         (typst-canvas-scroll-left 5))))
    (lambda () (typst-canvas-screenshot--save "screenshot-5.png"))
    (lambda ()
      (typst-canvas-screenshot--in-preview #'typst-canvas-zoom-fit)
      (with-current-buffer typst-canvas-screenshot--source
        (typst-canvas-screenshot--move-caret "e^(-x")))
    (lambda () (typst-canvas-screenshot--save "screenshot-6.png"))
    (lambda ()
      (with-current-buffer typst-canvas-screenshot--source
        (search-forward "sqrt(pi)")
        (insert " + #nope")))
    (lambda ()
      (with-current-buffer typst-canvas-screenshot--source
        (typst-canvas--update-caret)))
    (lambda () (typst-canvas-screenshot--save "screenshot-7.png"))
    (lambda ()
      (with-current-buffer typst-canvas-screenshot--source
        (delete-region (- (point) (length " + #nope")) (point))
        (set-buffer-modified-p nil))))))

;;; screenshot.el ends here
