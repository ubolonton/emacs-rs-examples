;;; latency.el --- Measure edit-to-screen latency, for bin/latency.sh -*- lexical-binding: t -*-

;; Opens examples/showcase.typ in a buffer that is not visiting the file, turns on
;; `typst-canvas-mode', then times edits on page 1: from the change in the buffer to the end of
;; the redisplay that shows the new pages.  Two kinds of edits:
;; - key: type one char into a word, then delete it.  Usually only page 1 changes.
;; - reflow: insert a call that adds a paragraph, then delete it.  All later pages move.
;; Prints one line per kind to stderr: median, min and max, in ms, of
;; - total: from the edit to the end of the redisplay.
;; - compile, render: as the module reports them, on its thread.
;; - lisp: the notification handler, which copies changed pages into canvases.
;; - display: the redisplay after it.

(require 'cl-lib)
(require 'typst-canvas)

(defconst typst-canvas-latency--root
  (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))

(defconst typst-canvas-latency--rounds 10
  "Edits of each kind.  Each round is an insertion and a deletion.")

(defconst typst-canvas-latency--timeout 60
  "Seconds after which the run fails.")

(defconst typst-canvas-latency--anchor "Click\nany word"
  "Text on page 1 where edits go, right before it.")

(defconst typst-canvas-latency--paragraph "#lorem(90) "
  "Inserted text that adds enough lines to page 1 to move the content of all later pages.")

(defun typst-canvas-latency--print (format-string &rest arguments)
  "Print FORMAT-STRING with ARGUMENTS to stderr, on its own line."
  (princ (concat (apply #'format format-string arguments) "\n") #'external-debugging-output))

(defun typst-canvas-latency--settled-p (sent)
  "Return non-nil if a request newer than SENT is served, and shown."
  (and typst-canvas--status
       (> typst-canvas--sent sent)
       (>= (car typst-canvas--status) typst-canvas--sent)
       (with-current-buffer typst-canvas--preview
         (and (> (length typst-canvas--serials) 0)
              (cl-every #'identity typst-canvas--serials)))))

(defun typst-canvas-latency--wait (sent)
  "Process output until the preview shows a request newer than SENT, then redisplay.
The edit sends its request from a timer, so wait for that too."
  (let ((deadline (+ (float-time) typst-canvas-latency--timeout)))
    (while (not (typst-canvas-latency--settled-p sent))
      (when (> (float-time) deadline)
        (typst-canvas-latency--print "Timed out")
        (kill-emacs 1))
      (accept-process-output nil 0.001))
    (redisplay t)))

(defvar typst-canvas-latency--handler-ms 0
  "Time spent in the notification handler since the last edit, in ms.")

(defun typst-canvas-latency--time-handler (handler &rest arguments)
  "Call HANDLER with ARGUMENTS, and add its time to `typst-canvas-latency--handler-ms'."
  (let ((start (float-time)))
    (prog1 (apply handler arguments)
      (cl-incf typst-canvas-latency--handler-ms (* 1000 (- (float-time) start))))))

(defun typst-canvas-latency--time (edit)
  "Call EDIT, and return (TOTAL COMPILE RENDER LISP DISPLAY) in ms, until it is on screen."
  (setq typst-canvas-latency--handler-ms 0)
  (let ((start (float-time))
        (sent typst-canvas--sent)
        display-start)
    (funcall edit)
    (cl-letf (((symbol-function 'redisplay)
               (let ((redisplay (symbol-function 'redisplay)))
                 (lambda (&rest arguments)
                   (setq display-start (float-time))
                   (apply redisplay arguments)))))
      (typst-canvas-latency--wait sent))
    (pcase-let ((`(,_served ,_pages ,_errors ,_warnings ,compile ,render) typst-canvas--status)
                (end (float-time)))
      (list (* 1000 (- end start)) compile render typst-canvas-latency--handler-ms
            (* 1000 (- end display-start))))))

(defun typst-canvas-latency--measure (insert)
  "Time `typst-canvas-latency--rounds' rounds of INSERT and the deletion of its text.
INSERT inserts text at point and returns it."
  (let (samples)
    (dotimes (_ typst-canvas-latency--rounds)
      (let (text)
        (push (typst-canvas-latency--time (lambda () (setq text (funcall insert)))) samples)
        (push (typst-canvas-latency--time (lambda () (delete-char (- (length text)))))
              samples)))
    samples))

(defun typst-canvas-latency--report (kind samples)
  "Print the median, min and max of SAMPLES, under the name KIND."
  (cl-flet ((stats (index)
              (let ((values (sort (mapcar (lambda (sample) (nth index sample)) samples) #'<)))
                (format "%5.1f (%5.1f..%5.1f)"
                        (nth (/ (length values) 2) values) (car values) (car (last values))))))
    (typst-canvas-latency--print
     "%-6s total %s  compile %s  render %s  lisp %s  display %s  [median (min..max) ms, %d edits]"
     kind (stats 0) (stats 1) (stats 2) (stats 3) (stats 4) (length samples))))

(defun typst-canvas-latency--run ()
  "Measure, report, and exit."
  (typst-canvas-latency--wait 0)
  (goto-char (point-min))
  (search-forward typst-canvas-latency--anchor)
  (goto-char (match-beginning 0))
  ;; "Click" becomes "Clicks" and back: the same line, so usually only page 1 changes.
  (forward-word)
  (typst-canvas-latency--report
   "key" (typst-canvas-latency--measure (lambda () (insert "s") "s")))
  (backward-word)
  (typst-canvas-latency--report
   "reflow" (typst-canvas-latency--measure
             (lambda () (insert typst-canvas-latency--paragraph) typst-canvas-latency--paragraph)))
  (set-buffer-modified-p nil)
  (kill-emacs 0))

(set-frame-size nil 1600 900 t)
(delete-other-windows)
(switch-to-buffer (get-buffer-create "*typst-canvas latency*"))
(insert-file-contents (expand-file-name "examples/showcase.typ" typst-canvas-latency--root))
(setq default-directory (expand-file-name "examples/" typst-canvas-latency--root))
(typst-canvas-mode 1)
(advice-add 'typst-canvas--on-notify :around #'typst-canvas-latency--time-handler)
;; Start after the frame is up, so that the preview has its final width.
(run-with-timer 1 nil #'typst-canvas-latency--run)

;;; latency.el ends here
