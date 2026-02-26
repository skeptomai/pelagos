;;; Remora Lisp standard macros — embedded at compile time, evaluated at startup.
;;;
;;; These macros are available in every .reml file without any import.

;; (define-service var-name "svc-name"
;;   :image    "img"
;;   :network  "net"
;;   :port     (host-var . container)            ; host maps to container
;;   :port     (8080 . 80) (2003 . 2003)         ; multiple ports under one :port
;;   :env      ("KEY1" . "val1") ("KEY2" . var)  ; host env maps to container env
;;   :bind     ("./host/path" . "/container/path") ; read-only (safe default)
;;   :bind-rw  ("./host/path" . "/container/path") ; read-write (explicit opt-in)
;;   :memory   mem-var)
;;
;; The options are a flat keyword-value list.  Each :keyword introduces a new
;; option; its values run until the next :keyword or end of list.
;;
;; If a keyword's values are all sublists (e.g. :env pairs), each sublist is
;; expanded into a separate service option:
;;   :env ("A" "1") ("B" "2")  →  (list 'env "A" "1") (list 'env "B" "2")
;;
;; If a keyword's values are atoms or mixed, they all go into one option:
;;   :port host 80             →  (list 'port host 80)
;;   :depends-on "redis" 6379  →  (list 'depends-on "redis" 6379)
;;
;; The practical reason proper lists are used for multi-value entries (rather
;; than dotted pairs) is that unquote-splicing `,@sub` only works on proper
;; lists.  Dotted pairs are semantically purer for key-value data but require
;; explicit (car)/(cdr) in the macro template instead of clean splice syntax.

(defmacro define-service (var-name svc-name . opts)

  ;; True if v is a :keyword symbol (starts with ':').
  (define (kw? v)
    (and (symbol? v)
         (string=? (substring (symbol->string v) 0 1) ":")))

  ;; Split a flat keyword-value list into groups.
  ;; (:a 1 2 :b 3 :c (x) (y)) → ((:a 1 2) (:b 3) (:c (x) (y)))
  (define (split-opts lst)
    (if (null? lst) '()
        (let loop ((rest   (cdr lst))
                   (kw     (car lst))
                   (vals   '())
                   (result '()))
          (cond
            ((null? rest)
             (reverse (cons (cons kw (reverse vals)) result)))
            ((kw? (car rest))
             (loop (cdr rest)
                   (car rest)
                   '()
                   (cons (cons kw (reverse vals)) result)))
            (else
             (loop (cdr rest) kw (cons (car rest) vals) result))))))

  ;; Convert one group (kw val...) into a list of (list 'key args...) forms.
  ;; Returns a list so multi-value groups (like :env) can yield multiple forms.
  ;;
  ;; Values may be:
  ;;   atoms/exprs  — all grouped into one option: :memory mem-var → (list 'memory mem-var)
  ;;   proper lists — one option per sublist, spliced: :env ("K" "v") → (list 'env "K" "v")
  ;;   dotted pairs — one option per pair, car/cdr:  :env ("K" . "v") → (list 'env "K" "v")
  (define (expand-opt group)
    (let* ((kw   (car group))
           (sym  (string->symbol (substring (symbol->string kw) 1)))
           (args (cdr group)))
      (if (and (not (null? args)) (pair? (car args)))
          ;; All values are pairs (proper or dotted) — one service option per sub-value.
          (map (lambda (sub)
                 (if (list? sub)
                     `(list ',sym ,@sub)              ; proper list: splice all items
                     `(list ',sym ,(car sub) ,(cdr sub)))) ; dotted pair: key . val
               args)
          ;; Atom values → a single service option with all args.
          (list `(list ',sym ,@args)))))

  `(define ,var-name
     (service ,svc-name ,@(apply append (map expand-opt (split-opts opts))))))

;; (assoc key alist) — find first pair in alist whose car equals key (string=?).
;; Returns the pair (key . value) or #f if not found.
(define (assoc key lst)
  (cond ((null? lst) #f)
        ((string=? key (car (car lst))) (car lst))
        (else (assoc key (cdr lst)))))

;; (result-ref results name) — extract a handle from a run-all alist by service name.
(define (result-ref results name)
  (let ((entry (assoc name results)))
    (if entry
      (cdr entry)
      (errorf "result-ref: no result for '~a'" name))))

;; (zero? x) — true if x is equal to 0.
(define (zero? x) (= x 0))

;; (logf fmt arg...) — format a string and log it.
;; Equivalent to (log (format fmt arg...)).
(defmacro logf (fmt . args)
  `(log (format ,fmt ,@args)))

;; (errorf fmt arg...) — format a string and raise it as an error.
;; Equivalent to (error (format fmt arg...)).
(defmacro errorf (fmt . args)
  `(error (format ,fmt ,@args)))

;; (unless condition body...)
;; Runs body when condition is false.  Equivalent to (when (not condition) body...).
(defmacro unless (condition . body)
  `(when (not ,condition) ,@body))

;; ── Result type ───────────────────────────────────────────────────────────
;;
;; A lightweight Result<T,E> type represented as a tagged list.
;;
;;   (ok value)    — success; wraps a return value
;;   (err reason)  — failure; wraps an error string
;;
;; Predicates: (ok? r)  (err? r)
;; Accessors:  (ok-value r)  (err-reason r)

(define (ok  value)  (list 'ok  value))
(define (err reason) (list 'err reason))

(define (ok?  r) (and (pair? r) (eq? (car r) 'ok)))
(define (err? r) (and (pair? r) (eq? (car r) 'err)))

(define (ok-value   r) (cadr r))
(define (err-reason r) (cadr r))

;; (with-cleanup cleanup body...)
;; Runs body; calls (cleanup result) on any exit where result is either
;; (ok value) on normal exit or (err reason) on error exit.
;; The error is re-raised after cleanup so it still propagates.
(defmacro with-cleanup (cleanup . body)
  `(guard (exn (#t (,cleanup (err exn)) (error exn)))
     (let ((result (begin ,@body)))
       (,cleanup (ok result))
       result)))

;; ── Future helpers ────────────────────────────────────────────────────────

;; (define-future name svc-spec [:after list] [:inject lambda])
;; Shorthand for (define name-fut (container-start-async svc-spec ...)).
;; The variable bound is name-fut; the service name comes from svc-spec.
(defmacro define-future (name svc . opts)
  (define fut-name (string->symbol (string-append (symbol->string name) "-fut")))
  `(define ,fut-name (container-start-async ,svc ,@opts)))

;; (define-futures (name svc-spec) ...)
;; Batch form of define-future; expands each clause independently.
(defmacro define-futures clauses
  `(begin
     ,@(map (lambda (clause)
              `(define-future ,(car clause) ,(cadr clause)))
            clauses)))

;; (define-transform name upstream body...)
;; Shorthand for (define name-fut (then upstream-fut (lambda (upstream) body...))).
;; Convention: name-fut is the output future; upstream-fut is the input future;
;; upstream is the lambda parameter bound to the resolved value.
(defmacro define-transform (name upstream . body)
  (define fut-name  (string->symbol (string-append (symbol->string name)     "-fut")))
  (define up-name   (string->symbol (string-append (symbol->string upstream) "-fut")))
  `(define ,fut-name (then ,up-name (lambda (,upstream) ,@body))))
